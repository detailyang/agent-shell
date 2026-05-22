use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::term_emulator::TermEmulator;
use crate::terminal::{enter_raw_mode_keep_signals, SigactionGuard};

/// Metadata header written as the first line of a recording file.
/// Identifies the terminal geometry and program at session creation time.
/// The `dir` field is always `"meta"` — replay uses this to distinguish
/// header lines from data events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingHeader {
    /// Always "meta".
    pub dir: String,
    /// Unix timestamp in milliseconds at session creation.
    pub ts: u64,
    /// Terminal rows at session creation.
    pub rows: u16,
    /// Terminal columns at session creation.
    pub cols: u16,
    /// argv[0] of the launched program.
    pub program: String,
}

/// A single recording event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingEvent {
    /// Unix timestamp in milliseconds
    pub ts: u64,
    /// Direction: "in" (written to PTY) or "out" (PTY output)
    pub dir: String,
    /// Base64-encoded raw bytes
    pub data: String,
}

/// Recording handle. Writes NDJSON events to a file.
pub struct Recording {
    file: File,
}

impl Recording {
    /// Open a recording file at the given path.
    pub fn new(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Use create_new to avoid appending to a stale file from a prior run that
        // happened to reuse the same 8-char session ID.  If the file already exists
        // we truncate it so the header is always line 1 of a fresh recording.
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        Ok(Recording { file })
    }

    /// Record input (bytes written to PTY).
    pub fn record_in(&mut self, bytes: &[u8]) {
        self.record("in", bytes);
    }

    /// Record output (bytes from PTY).
    pub fn record_out(&mut self, bytes: &[u8]) {
        self.record("out", bytes);
    }

    /// Record a terminal resize event.
    ///
    /// Encodes the new dimensions as `"{rows},{cols}"` in the `data` field
    /// (base64-encoded, like all other events).  Replay uses `dir == "resize"`
    /// to detect these and update the TermEmulator grid accordingly.
    pub fn record_resize(&mut self, rows: u16, cols: u16) {
        let size_str = format!("{},{}", rows, cols);
        self.record("resize", size_str.as_bytes());
    }

    fn record(&mut self, dir: &str, bytes: &[u8]) {
        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let event = RecordingEvent {
            ts,
            dir: dir.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        };

        if let Ok(line) = serde_json::to_string(&event) {
            let _ = writeln!(self.file, "{}", line);
            // Flush after every event so a crash doesn't lose recent data.
            let _ = self.file.flush();
        }
    }

    /// Write the session metadata header as the first line of the recording.
    ///
    /// Must be called once immediately after `Recording::new`, before any
    /// `record_in` / `record_out` calls, so the header is always line 1.
    pub fn write_header(&mut self, rows: u16, cols: u16, program: &str) {
        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let header = RecordingHeader {
            dir: "meta".to_string(),
            ts,
            rows,
            cols,
            program: program.to_string(),
        };
        if let Ok(line) = serde_json::to_string(&header) {
            let _ = writeln!(self.file, "{}", line);
            let _ = self.file.flush();
        }
    }

    /// Flush and close the recording file.
    pub fn close(&mut self) {
        let _ = self.file.flush();
    }
}

/// Replay options.
pub struct ReplayOptions {
    pub speed: f64,
    pub dump: bool,
    pub force: bool,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        ReplayOptions {
            speed: 1.0,
            dump: false,
            force: false,
        }
    }
}

/// Replay a recording file to stdout.
pub fn replay(file: PathBuf, opts: ReplayOptions) -> std::io::Result<()> {
    let f = File::open(&file)?;
    let reader = BufReader::new(f);

    let is_terminal = unsafe { libc::isatty(1) != 0 };

    if opts.dump {
        if is_terminal && !opts.force {
            eprintln!("Warning: --dump outputs raw bytes to stdout. Use --force to confirm.");
            std::process::exit(1);
        }
        dump_replay(reader)
    } else {
        timed_replay(reader, opts.speed)
    }
}

fn dump_replay(reader: BufReader<File>) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in reader.lines() {
        let line = line?;
        if let Ok(event) = serde_json::from_str::<RecordingEvent>(&line) {
            if event.dir == "out" {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&event.data)
                    .unwrap_or_default();
                out.write_all(&bytes)?;
            }
        }
    }
    out.flush()?;
    Ok(())
}

// ─── Replay helpers ──────────────────────────────────────────────────

/// Parse the first line of a recording file as a `RecordingHeader`.
/// Returns `None` if the line is not a meta header (old-format file).
pub fn read_header(line: &str) -> Option<RecordingHeader> {
    let header: RecordingHeader = serde_json::from_str(line).ok()?;
    if header.dir == "meta" {
        Some(header)
    } else {
        None
    }
}

/// Remove terminal query sequences from a byte buffer before writing to the
/// replay terminal.
///
/// Sequences removed:
/// - `\x1b[6n`          DSR (Device Status Report) — triggers CPR response
/// - `\x1bP...\x1b\\`  DCS (Device Control String) — triggers DCS response
///
/// OSC title sequences (`\x1b]0;...\x07`) are intentionally kept: they set
/// the window title but do not cause the terminal to write to stdin.
pub fn filter_query_sequences(buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        // DSR: \x1b[6n  (3 bytes: ESC [ 6 n)
        if buf[i] == 0x1b
            && i + 3 < buf.len()
            && buf[i + 1] == b'['
            && buf[i + 2] == b'6'
            && buf[i + 3] == b'n'
        {
            i += 4; // skip entirely
            continue;
        }
        // DCS: \x1bP...\x1b\\  (ESC P ... ESC \\)
        if buf[i] == 0x1b && i + 1 < buf.len() && buf[i + 1] == b'P' {
            // Find String Terminator: ESC \\
            let st_pos = buf[i + 2..]
                .windows(2)
                .position(|w| w == b"\x1b\\");
            if let Some(rel) = st_pos {
                i += 2 + rel + 2; // skip DCS + ST
                continue;
            }
            // No ST found — malformed DCS. Skip just the two-byte ESC P header
            // and continue scanning so subsequent bytes are not silently dropped.
            i += 2;
            continue;
        }
        out.push(buf[i]);
        i += 1;
    }
    out
}

/// Detect whether a recording's output byte stream uses alternate screen
/// (i.e. contains `ESC[?1049h`).  Used by `timed_replay` to choose between
/// the TermEmulator path (alt-screen programs like vim/htop) and the raw-
/// passthrough path (inline TUI programs like pi that never enter smcup).
pub fn recording_uses_alt_screen(events: &[RecordingEvent]) -> bool {
    for ev in events {
        if ev.dir != "out" { continue; }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&ev.data)
            .unwrap_or_default();
        if bytes.windows(8).any(|w| w == b"\x1b[?1049h") {
            return true;
        }
    }
    false
}

/// Normalise PTY double-CR line endings produced by inline TUI apps.
///
/// When a program running in a PTY writes `\r\n`, the PTY line discipline
/// (ONLCR) translates the `\n` to `\r\n`, yielding `\r\r\n` in the output
/// stream that gets recorded.  When this is replayed to a real terminal the
/// terminal's own ONLCR would turn the final `\n` into `\r\n` again, giving
/// `\r\r\r\n` — an extra blank column shift on every line.
///
/// This function collapses every `\r\r\n` → `\r\n` so the bytes replayed to
/// the terminal are identical to what the application originally intended.
pub fn normalize_crlf(buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        // Collapse \r\r\n → \r\n
        if i + 2 < buf.len()
            && buf[i] == b'\r'
            && buf[i + 1] == b'\r'
            && buf[i + 2] == b'\n'
        {
            out.push(b'\r');
            out.push(b'\n');
            i += 3;
            continue;
        }
        out.push(buf[i]);
        i += 1;
    }
    out
}

/// Heuristic terminal size inference from raw PTY output bytes.
///
/// Strategy:
/// - rows: the `bottom` value from the first `\x1b[{top};{bottom}r` scroll
///   region sequence (TUI apps set this to the terminal height on startup).
/// - cols: the maximum `col` value seen in any `\x1b[{row};{col}H` cursor
///   position sequence.
///
/// Returns `None` if neither can be determined.
pub fn heuristic_size(bytes: &[u8]) -> Option<(u16, u16)> {
    let mut rows: Option<u16> = None;
    let mut max_col: u16 = 0;
    // Tracks the maximum width observed in SGR-bracketed padding runs.
    // vim pads both the tilde filler lines and the status bar to exactly
    // `cols` characters; measuring these gives a reliable cols estimate
    // even when \x1b[row;colH absolute positions only reach a small column.
    let mut max_padded_run: u16 = 0;

    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b || i + 1 >= bytes.len() || bytes[i + 1] != b'[' {
            i += 1;
            continue;
        }
        // Collect CSI parameter bytes (0x30-0x3f) then read the final byte.
        let param_start = i + 2;
        let mut j = param_start;
        while j < bytes.len() && bytes[j] >= 0x20 && bytes[j] <= 0x3f {
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let final_byte = bytes[j];
        let param_str = std::str::from_utf8(&bytes[param_start..j]).unwrap_or("");

        match final_byte {
            // \x1b[{top};{bottom}r  — set scroll region
            b'r' if rows.is_none() => {
                let mut parts = param_str.splitn(2, ';');
                let _top = parts.next();
                if let Some(bottom_str) = parts.next() {
                    if let Ok(bottom) = bottom_str.parse::<u16>() {
                        if bottom > 0 {
                            rows = Some(bottom);
                        }
                    }
                }
            }
            // \x1b[{row};{col}H  — cursor position
            b'H' | b'f' => {
                let mut parts = param_str.splitn(2, ';');
                let _row = parts.next();
                if let Some(col_str) = parts.next() {
                    if let Ok(col) = col_str.parse::<u16>() {
                        if col > max_col {
                            max_col = col;
                        }
                    }
                }
            }
            // SGR (\x1b[...m): measure the printable run that follows.
            //
            // TUI apps like vim pad filler lines ("~") and the status bar
            // to exactly `cols` characters using a pattern of:
            //   ESC[<attr>m <printable bytes> ESC[<next>m
            // Measuring the longest such run gives a reliable cols estimate
            // because these lines must fill the full terminal width.
            // We only count printable ASCII (0x20-0x7e) and UTF-8 lead bytes
            // (≥0x80) to avoid counting control characters.
            b'm' => {
                let run_start = j + 1;
                let mut k = run_start;
                while k < bytes.len() && bytes[k] != 0x1b && bytes[k] != b'\r' && bytes[k] != b'\n' {
                    k += 1;
                }
                // Only count if the run ends at another ESC (next SGR),
                // not at a CR/LF (which would be mid-line content, not padding).
                if k < bytes.len() && bytes[k] == 0x1b {
                    let run_len = (k - run_start) as u16;
                    if run_len > max_padded_run {
                        max_padded_run = run_len;
                    }
                }
            }
            _ => {}
        }
        i = j + 1;
    }

    // cols: prefer the padded-run measurement (most reliable for TUI apps)
    // over the max cursor-H column (which only reaches as far as the last
    // absolute cursor command, often much less than the terminal width).
    let cols = if max_padded_run > max_col {
        max_padded_run
    } else {
        max_col
    };

    match (rows, cols) {
        (Some(r), c) if c > 0 => Some((r, c)),
        (Some(r), _) => Some((r, 80)), // cols unknown, fall back to 80
        (None, c) if c > 0 => Some((24, c)), // rows unknown, fall back to 24
        _ => None,
    }
}

/// Events whose timestamps differ by less than this threshold (in real time,
/// before speed scaling) are merged into a single write + flush.
///
/// Why this matters: the PTY kernel may split a single logical output frame
/// across two consecutive read() calls (e.g. at a 1024-byte boundary),
/// producing two recording events that are 0–2 ms apart in wall time.
/// If we flush between them the terminal sees a truncated escape sequence
/// in the first chunk (e.g. `ESC[38;5;130` without the trailing `m`),
/// which corrupts rendering. Merging these micro-bursts restores the
/// uninterrupted byte stream the terminal saw during the original session.
const MERGE_THRESHOLD_MS: u64 = 5;

/// Rewrite absolute terminal coordinates in `buf` so that the content is
/// rendered inside a viewport that starts at (`row_offset`, `col_offset`)
/// (both 1-based, same convention as VT100).
///
/// Every CSI sequence whose final byte implies a screen position is adjusted:
///
/// | Sequence          | Transformation                                      |
/// |-------------------|-----------------------------------------------------|
/// | `CSI row ; col H` | row += row_offset-1, col += col_offset-1  (CUP)    |
/// | `CSI row ; col f` | same as H  (HVP)                                   |
/// | `CSI top ; bot r` | top += row_offset-1, bot += row_offset-1 (DECSTBM) |
/// | `CSI n A`         | passed through unchanged (relative, safe)          |
/// | `CSI n B`         | passed through unchanged                           |
/// | `CSI n C`         | passed through unchanged                           |
/// | `CSI n D`         | passed through unchanged                           |
///
/// All other bytes (SGR, erase, mode set/reset, text) are passed through
/// unchanged, so colours and attributes are fully preserved.
///
/// `row_offset` and `col_offset` are 1-based. Passing (1, 1) is a no-op.
pub fn remap_coordinates(buf: &[u8], row_offset: u16, col_offset: u16) -> Vec<u8> {
    // Fast path: no remapping needed.
    if row_offset == 1 && col_offset == 1 {
        return buf.to_vec();
    }

    let mut out = Vec::with_capacity(buf.len() + 32);
    let mut i = 0;

    while i < buf.len() {
        // Only intercept ESC [ (CSI) sequences.
        if buf[i] != 0x1b || i + 1 >= buf.len() || buf[i + 1] != b'[' {
            out.push(buf[i]);
            i += 1;
            continue;
        }

        // Collect parameter bytes (0x30–0x3f) and the final byte (0x40–0x7e).
        let param_start = i + 2;
        let mut j = param_start;
        while j < buf.len() && (0x20..=0x3f).contains(&buf[j]) {
            j += 1;
        }
        if j >= buf.len() {
            // Truncated sequence: emit as-is.
            out.extend_from_slice(&buf[i..]);
            break;
        }
        let final_byte = buf[j];
        let param_str = std::str::from_utf8(&buf[param_start..j]).unwrap_or("");

        match final_byte {
            // CUP / HVP: \x1b[row;colH  or  \x1b[row;colF
            b'H' | b'f' => {
                let mut parts = param_str.splitn(2, ';');
                let row: u16 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                let col: u16 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                let new_row = row.saturating_add(row_offset - 1);
                let new_col = col.saturating_add(col_offset - 1);
                out.extend_from_slice(
                    format!("\x1b[{};{}{}" , new_row, new_col, final_byte as char).as_bytes(),
                );
            }
            // DECSTBM: \x1b[top;botr  — set scroll region
            b'r' => {
                let mut parts = param_str.splitn(2, ';');
                let top: u16 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                let bot: u16 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                let new_top = top.saturating_add(row_offset - 1);
                let new_bot = bot.saturating_add(row_offset - 1);
                out.extend_from_slice(
                    format!("\x1b[{};{}r", new_top, new_bot).as_bytes(),
                );
            }
            // Everything else: pass through byte-for-byte.
            _ => {
                out.extend_from_slice(&buf[i..=j]);
            }
        }
        i = j + 1;
    }

    out
}

fn timed_replay(reader: BufReader<File>, speed: f64) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // Load all lines, split header from data events.
    let mut lines = reader.lines();

    // Peek at the first line to detect a metadata header.
    let first_line = lines.next().and_then(|l| l.ok());
    let header: Option<RecordingHeader> = first_line
        .as_deref()
        .and_then(read_header);

    // Collect the remainder into RecordingEvents.  If the first line was NOT
    // a header, put it back by chaining it with the rest.
    let event_lines: Box<dyn Iterator<Item = String>> = if header.is_some() {
        Box::new(lines.map_while(Result::ok))
    } else {
        let rest = lines.map_while(Result::ok);
        match first_line {
            Some(fl) => Box::new(std::iter::once(fl).chain(rest)),
            None => Box::new(rest),
        }
    };

    // Pre-load all data events so we can peek at the next timestamp.
    let events: Vec<RecordingEvent> = event_lines
        .filter_map(|l| serde_json::from_str::<RecordingEvent>(&l).ok())
        .collect();

    // Resolve recording terminal size.
    // Priority: header > heuristic (from output bytes) > fallback 80x24.
    //
    // When a header exists but the recording lacks resize events (pre-fix
    // recordings), the TUI may have rendered at a larger size than the
    // header claims.  We run the heuristic unconditionally for alt-screen
    // recordings and take the max of header and heuristic dimensions so
    // the TermEmulator grid is large enough to contain all addressed cells.
    let is_tty = unsafe { libc::isatty(1) != 0 };

    // Collect all "out" bytes for the heuristic.
    let all_out: Vec<u8> = events
        .iter()
        .filter(|e| e.dir == "out")
        .flat_map(|e| {
            base64::engine::general_purpose::STANDARD
                .decode(&e.data)
                .unwrap_or_default()
        })
        .collect();
    let heuristic = heuristic_size(&all_out);

    let rec_size: (u16, u16) = if let Some(ref h) = header {
        // If the recording has resize events, trust the header as initial
        // size — the emulator will be resized dynamically.  Otherwise,
        // take the max of header and heuristic to cover recordings made
        // before resize events were recorded.
        let has_resize_events = events.iter().any(|e| e.dir == "resize");
        if has_resize_events {
            (h.rows, h.cols)
        } else if let Some((hr, hc)) = heuristic {
            (h.rows.max(hr), h.cols.max(hc))
        } else {
            (h.rows, h.cols)
        }
    } else {
        heuristic.unwrap_or((24, 80))
    };

    // Detect rendering mode once, before entering the replay loop.
    //
    // Alt-screen programs (vim, htop, …) switch to the alternate screen with
    // `ESC[?1049h` and do absolute cursor addressing within that fixed grid.
    // For these, feeding bytes into a TermEmulator and calling full_redraw()
    // per burst produces a correct, flicker-free result.
    //
    // Inline TUI programs (pi, lazygit without alt-screen, …) never call
    // smcup.  They rely on in-place cursor movement (ESC[nA / ESC[2K) and
    // ONLCR line endings to scroll the terminal naturally.  Running their
    // output through full_redraw() — which emits ESC[H ESC[2J on every
    // burst — clears the screen each frame and produces the "scrolling"
    // artefact the user sees.  For these we pass filtered raw bytes directly
    // to stdout, which is identical to what `--dump` does but with timing.
    let uses_alt_screen = recording_uses_alt_screen(&events);

    // Create a terminal emulator at the recording's original dimensions.
    // Only used when uses_alt_screen is true.
    let mut emu = TermEmulator::new(rec_size.0, rec_size.1);

    // Save the current terminal size so we can restore it when replay ends.
    // Then resize the terminal to the recording's original dimensions so
    // full_redraw() frames match the physical screen.
    let saved_size: Option<(u16, u16)> = if is_tty {
        let orig = crate::terminal::terminal_size();
        crate::terminal::set_terminal_size(rec_size.0, rec_size.1);
        orig
    } else {
        None
    };

    // ── Signal handling ────────────────────────────────────────────────────
    // Install a SIGINT handler that sets an atomic flag.  We use
    // `enter_raw_mode_keep_signals` (not the full cfmakeraw) so the terminal
    // still converts Ctrl-C into SIGINT.  The inter-frame sleep is split into
    // short slices so the flag is checked frequently.
    let interrupted = Arc::new(AtomicBool::new(false));
    let interrupted_flag = interrupted.clone();

    // Static trampoline: the handler stores a pointer to the Arc's inner bool.
    // We use a global atomic pointer rather than a closure because signal
    // handlers must be async-signal-safe (no allocation, no locks).
    static INTERRUPTED_PTR: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    INTERRUPTED_PTR.store(
        Arc::as_ptr(&interrupted_flag) as usize,
        Ordering::Release,
    );

    extern "C" fn sigint_handler(_: libc::c_int) {
        let ptr = INTERRUPTED_PTR.load(std::sync::atomic::Ordering::Acquire);
        if ptr != 0 {
            // SAFETY: the pointer is valid for the duration of the replay
            // (the Arc keeps it alive; we clear INTERRUPTED_PTR before drop).
            unsafe { (*(ptr as *const AtomicBool)).store(true, Ordering::Release); }
        }
    }

    // Install unconditionally — SIGINT via `kill -INT` works regardless of
    // whether stdout is a tty.  The raw-mode / ISIG path only matters for
    // Ctrl-C typed in an interactive terminal.
    let _sigint_guard: Option<SigactionGuard> = {
        use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
        let new_action = SigAction::new(
            SigHandler::Handler(sigint_handler),
            SaFlags::SA_RESTART,
            SigSet::empty(),
        );
        // SAFETY: handler is async-signal-safe (single atomic store).
        unsafe { sigaction(Signal::SIGINT, &new_action).ok() }
            .map(|old| SigactionGuard { sig: Signal::SIGINT, old })
    };

    // Use the signals-preserving raw mode so Ctrl-C still sends SIGINT.
    let _raw_guard = if is_tty { enter_raw_mode_keep_signals() } else { None };

    // ── Ctrl-D / stdin-EOF watcher thread ─────────────────────────────────
    // Ctrl-D is not a signal: the terminal driver converts it to an EOF
    // condition on the TTY (read() returns 0), or the raw byte 0x04 arrives
    // in the process's stdin buffer.  Neither path triggers SIGINT, so we
    // need to actively read stdin.
    //
    // We do this on a dedicated background thread because the main thread
    // holds stdout.lock() during replay and must not block on stdin.read().
    // The thread polls stdin in non-blocking mode (10 ms sleep between polls)
    // and sets `interrupted` on Ctrl-D / EOF / any read error.
    //
    // `replay_done` is a second flag the main thread sets when it finishes
    // (naturally or via interruption) so the watcher thread can exit cleanly.
    let replay_done = Arc::new(AtomicBool::new(false));
    // Only watch stdin when it is a TTY. When stdin is /dev/null or a pipe
    // (e.g. replay invoked as a subprocess), read() returns 0 (EOF)
    // immediately, which would spuriously set `interrupted` and abort the
    // replay before all events are processed.
    let stdin_is_tty = unsafe { libc::isatty(0) != 0 };
    let stdin_watcher = if stdin_is_tty {
        let interrupted = interrupted.clone();
        let replay_done = replay_done.clone();

        Some(std::thread::spawn(move || {
            // Use poll(2) with a short timeout to wait for stdin readability
            // WITHOUT setting O_NONBLOCK on the file description.
            //
            // Why not O_NONBLOCK?
            // In a PTY environment (ssh, IDE terminal, `script`, agent-shell
            // attach) fd 0 (stdin) and fd 1 (stdout) share the same open file
            // description (same pty slave).  O_NONBLOCK is a property of the
            // file description, not the fd, so setting it on fd 0 would also
            // make writes on fd 1 non-blocking.  That causes `write_all` to
            // the replay stdout to return EAGAIN (os error 35) when the kernel
            // buffer is momentarily full.
            //
            // poll() only inspects readiness; it never changes any fd flag.
            let mut pfd = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            let mut buf = [0u8; 64];

            loop {
                if replay_done.load(Ordering::Acquire) { return; }

                // Wait up to 20 ms for stdin to become readable.
                // SAFETY: pfd is a valid, fully-initialised pollfd.
                let ready = unsafe { libc::poll(&mut pfd, 1, 20) };

                if ready < 0 {
                    // poll() itself failed (e.g. EINTR from our SIGINT handler).
                    // EINTR is harmless — just retry.  Any other error is
                    // unexpected; treat as EOF to unblock the main thread.
                    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                    if err != libc::EINTR {
                        interrupted.store(true, Ordering::Release);
                        return;
                    }
                    continue;
                }

                if ready == 0 {
                    // Timeout: no data yet, loop and re-check replay_done.
                    continue;
                }

                // stdin is readable: do a single read.
                // SAFETY: fd 0 is stdin, buf is a valid writable slice.
                let n = unsafe {
                    libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                };

                if n == 0 {
                    // EOF — Ctrl-D on an empty TTY input buffer, or pipe closed.
                    interrupted.store(true, Ordering::Release);
                    return;
                } else if n > 0 {
                    // Got bytes: check for Ctrl-D byte (0x04).
                    if buf[..n as usize].contains(&0x04) {
                        interrupted.store(true, Ordering::Release);
                        return;
                    }
                    // Any other key is ignored (replay is read-only).
                } else {
                    // n < 0: unexpected read error — treat as EOF.
                    interrupted.store(true, Ordering::Release);
                    return;
                }
            }
        }))
    } else {
        None
    };

    // Interruptible sleep: break into 10 ms slices so the interrupted flag
    // is polled at most 10 ms after SIGINT arrives.
    let sleep_interruptible = |total_ms: u64, interrupted: &AtomicBool| -> bool {
        const SLICE_MS: u64 = 10;
        let mut remaining = total_ms;
        while remaining > 0 {
            if interrupted.load(Ordering::Acquire) {
                return true; // interrupted
            }
            let this_slice = remaining.min(SLICE_MS);
            std::thread::sleep(std::time::Duration::from_millis(this_slice));
            remaining = remaining.saturating_sub(this_slice);
        }
        interrupted.load(Ordering::Acquire)
    };

    // Replay loop.
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut last_flush_ts: Option<u64> = None;

    let result = (|| -> std::io::Result<()> {
        let mut i = 0;
        while i < events.len() {
            // Check interrupt before every frame.
            if interrupted.load(Ordering::Acquire) {
                break;
            }

            let event = &events[i];

            // Inter-event delay: sleep for the wall-clock gap since the last
            // flushed frame, scaled by speed.
            if let Some(prev_ts) = last_flush_ts {
                let raw_delta_ms = event.ts.saturating_sub(prev_ts);
                let scaled_ms = (raw_delta_ms as f64 / speed) as u64;
                if scaled_ms > 0 {
                    if sleep_interruptible(scaled_ms, &interrupted) {
                        break;
                    }
                }
            }

            // Merge micro-burst: accumulate consecutive out-events within
            // MERGE_THRESHOLD_MS into a single buffer, then process once.
            // Input events are only used for timing; their bytes are not replayed.
            let mut write_buf: Vec<u8> = Vec::new();
            let mut group_end_ts;

            let mut j = i;
            loop {
                let ev = &events[j];

                if ev.dir == "out" {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&ev.data)
                        .unwrap_or_default();
                    write_buf.extend_from_slice(&bytes);
                } else if ev.dir == "resize" {
                    // Flush any accumulated output before resizing so the
                    // emulator processes those bytes at the old dimensions.
                    if uses_alt_screen && !write_buf.is_empty() {
                        emu.process(&write_buf);
                        let frame = emu.full_redraw();
                        out.write_all(&frame)?;
                        out.flush()?;
                        write_buf.clear();
                    }
                    // Parse "{rows},{cols}" from the resize event data.
                    if let Ok(size_bytes) = base64::engine::general_purpose::STANDARD
                        .decode(&ev.data)
                    {
                        if let Ok(size_str) = std::str::from_utf8(&size_bytes) {
                            let parts: Vec<&str> = size_str.split(',').collect();
                            if parts.len() == 2 {
                                if let (Ok(r), Ok(c)) = (
                                    parts[0].parse::<u16>(),
                                    parts[1].parse::<u16>(),
                                ) {
                                    emu.resize(r, c);
                                    if is_tty {
                                        crate::terminal::set_terminal_size(r, c);
                                    }
                                }
                            }
                        }
                    }
                }

                group_end_ts = ev.ts;

                // Peek at the next event: if it arrives within the merge
                // window, pull it into this group regardless of direction.
                if j + 1 < events.len() {
                    let next_delta = events[j + 1].ts.saturating_sub(ev.ts);
                    if next_delta < MERGE_THRESHOLD_MS {
                        j += 1;
                        continue;
                    }
                }
                break;
            }

            if !write_buf.is_empty() {
                if uses_alt_screen {
                    // Alt-screen program: feed into TermEmulator and emit a
                    // full redraw so the client terminal sees a coherent frame
                    // regardless of its own scroll position.
                    emu.process(&write_buf);
                    let frame = emu.full_redraw();
                    out.write_all(&frame)?;
                } else {
                    // Inline TUI program: pass filtered raw bytes directly.
                    // filter_query_sequences strips DSR/DCS that would cause
                    // the terminal to write back to stdin.
                    // normalize_crlf collapses \r\r\n → \r\n: the recording
                    // contains the double-CR produced by PTY ONLCR; the replay
                    // terminal would apply ONLCR again, giving \r\r\r\n.
                    let filtered = filter_query_sequences(&write_buf);
                    let normalized = normalize_crlf(&filtered);
                    out.write_all(&normalized)?;
                }
                out.flush()?;
            }

            last_flush_ts = Some(group_end_ts);
            i = j + 1;
        }
        out.flush()?;
        Ok(())
    })();

    // Signal the stdin watcher thread to exit and wait for it.
    // Must happen before dropping the `interrupted` Arc so the thread's
    // clone of the Arc is the last reference to keep the flag alive.
    replay_done.store(true, Ordering::Release);
    if let Some(watcher) = stdin_watcher {
        let _ = watcher.join();
    }

    // Deactivate signal handler before dropping the Arc so the pointer
    // stored in INTERRUPTED_PTR is no longer dereferenced.
    INTERRUPTED_PTR.store(0, Ordering::Release);
    drop(_sigint_guard); // restores previous SIGINT action (SIG_DFL)

    // Cleanup.
    if is_tty {
        if uses_alt_screen {
            // Leave alternate screen and reset attributes.
            let _ = out.write_all(b"\x1b[?1049l");
        }
        let _ = out.write_all(b"\x1b[0m");
        let _ = out.flush();
        // Restore the original terminal size.
        if let Some((r, c)) = saved_size {
            crate::terminal::set_terminal_size(r, c);
        }
        // _raw_guard dropped here → termios restored via RAII.
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── read_header ───────────────────────────────────────────────

    #[test]
    fn read_header_recognises_meta_line() {
        let line = r#"{"dir":"meta","ts":1000,"rows":24,"cols":80,"program":"bash"}"#;
        let h = read_header(line).expect("should parse as header");
        assert_eq!(h.dir, "meta");
        assert_eq!(h.rows, 24);
        assert_eq!(h.cols, 80);
        assert_eq!(h.program, "bash");
    }

    #[test]
    fn read_header_returns_none_for_data_line() {
        // Old-format file: first line is an "out" event, not "meta".
        let line = r#"{"dir":"out","ts":1000,"data":"aGVsbG8="}"#;
        assert!(read_header(line).is_none());
    }

    // ─── filter_query_sequences ──────────────────────────────────────

    #[test]
    fn filter_removes_dsr() {
        // DSR \x1b[6n must be stripped; surrounding bytes must survive.
        let input = b"before\x1b[6nafter";
        let out = filter_query_sequences(input);
        assert_eq!(out, b"beforeafter");
    }

    #[test]
    fn filter_removes_multiple_dsr() {
        let input = b"\x1b[6n\x1b[6n";
        let out = filter_query_sequences(input);
        assert!(out.is_empty());
    }

    #[test]
    fn filter_removes_dcs() {
        // DCS \x1bPzz\x1b\\ must be stripped entirely.
        let input = b"before\x1bPzz\x1b\\after";
        let out = filter_query_sequences(input);
        assert_eq!(out, b"beforeafter");
    }

    #[test]
    fn filter_keeps_osc_title() {
        // OSC title set does not trigger a stdin response; keep it intact.
        let input = b"\x1b]0;my title\x07rest";
        let out = filter_query_sequences(input);
        assert_eq!(out, input);
    }

    #[test]
    fn filter_keeps_normal_sgr() {
        // SGR and cursor-move sequences must pass through unchanged.
        let input = b"\x1b[38;5;130mhello\x1b[0m";
        let out = filter_query_sequences(input);
        assert_eq!(out, input);
    }

    // ─── normalize_crlf ───────────────────────────────────────────

    #[test]
    fn normalize_collapses_double_cr() {
        // PTY ONLCR turns \r\n into \r\r\n in the recording.
        // normalize_crlf must collapse it back to \r\n so the replay
        // terminal's own ONLCR does not produce a third \r.
        let input = b"line1\r\r\nline2\r\r\n";
        let out = normalize_crlf(input);
        assert_eq!(out, b"line1\r\nline2\r\n");
    }

    #[test]
    fn normalize_preserves_single_crlf() {
        // A plain \r\n (no double CR) must pass through unchanged.
        let input = b"hello\r\nworld\r\n";
        let out = normalize_crlf(input);
        assert_eq!(out, b"hello\r\nworld\r\n");
    }

    #[test]
    fn normalize_preserves_standalone_cr() {
        // A bare \r not followed by another \r\n must pass through.
        let input = b"\rhello";
        let out = normalize_crlf(input);
        assert_eq!(out, b"\rhello");
    }

    #[test]
    fn normalize_mixed() {
        // Mix of \r\r\n and \r\n in the same buffer.
        let input = b"a\r\r\nb\r\nc\r\r\n";
        let out = normalize_crlf(input);
        assert_eq!(out, b"a\r\nb\r\nc\r\n");
    }

    // ─── recording_uses_alt_screen ───────────────────────────────────

    #[test]
    fn detects_alt_screen_present() {
        let ev = RecordingEvent {
            ts: 0, dir: "out".into(),
            data: base64::engine::general_purpose::STANDARD
                .encode(b"\x1b[?1049hsome content"),
        };
        assert!(recording_uses_alt_screen(&[ev]));
    }

    #[test]
    fn detects_alt_screen_absent() {
        // Inline TUI (pi-style): no smcup, only cursor-up + erase-line.
        let ev = RecordingEvent {
            ts: 0, dir: "out".into(),
            data: base64::engine::general_purpose::STANDARD
                .encode(b"\x1b[3A\r\x1b[2Kcontent\r\r\n"),
        };
        assert!(!recording_uses_alt_screen(&[ev]));
    }

    #[test]
    fn detects_alt_screen_ignores_in_events() {
        // ESC[?1049h in an "in" (input) event must not count.
        let ev = RecordingEvent {
            ts: 0, dir: "in".into(),
            data: base64::engine::general_purpose::STANDARD
                .encode(b"\x1b[?1049h"),
        };
        assert!(!recording_uses_alt_screen(&[ev]));
    }

    // ─── heuristic_size ────────────────────────────────────────────

    #[test]
    fn heuristic_infers_rows_from_scroll_region() {
        // \x1b[1;24r sets scroll region rows 1-24 → infer rows=24.
        let input = b"\x1b[1;24r";
        let (rows, _) = heuristic_size(input).expect("should infer size");
        assert_eq!(rows, 24);
    }

    #[test]
    fn heuristic_infers_cols_from_cursor_position() {
        // \x1b[1;80H → col=80.
        let input = b"\x1b[1;80H";
        let (_, cols) = heuristic_size(input).expect("should infer size");
        assert_eq!(cols, 80);
    }

    #[test]
    fn heuristic_uses_max_col() {
        // Multiple cursor position commands — max col wins.
        let input = b"\x1b[1;40H\x1b[5;120H\x1b[1;24r";
        let (rows, cols) = heuristic_size(input).expect("should infer size");
        assert_eq!(rows, 24);
        assert_eq!(cols, 120);
    }

    /// Regression: vim uses \r\n for most line breaks, so absolute cursor
    /// positions only reach a small column (e.g. col 22 for an 80-col terminal).
    /// heuristic_size must infer cols from SGR-padded runs (tilde lines, status
    /// bar) rather than from cursor-H positions alone.
    #[test]
    fn heuristic_infers_cols_from_sgr_padded_run() {
        // Simulate a vim tilde filler line: ESC[94m + '~' + 79 spaces + ESC[0m
        // This is exactly how vim fills empty lines to the terminal width.
        // The padded run length (80 chars) must win over a small cursor-H col.
        let mut input = Vec::new();
        // Scroll region: rows=24
        input.extend_from_slice(b"\x1b[1;24r");
        // A cursor position reaching only col 22 (as seen in bfd784c5 / 7f6e09ca)
        input.extend_from_slice(b"\x1b[3;22H");
        // Tilde filler line padded to 80 cols
        input.extend_from_slice(b"\x1b[94m");
        input.push(b'~');
        input.extend(std::iter::repeat(b' ').take(79)); // total 80 printable chars
        input.extend_from_slice(b"\x1b[0m");

        let (rows, cols) = heuristic_size(&input).expect("should infer size");
        assert_eq!(rows, 24);
        assert_eq!(cols, 80, "cols must come from tilde padding, not cursor-H col 22");
    }

    #[test]
    fn heuristic_returns_none_for_plain_text() {
        // No escape sequences at all — cannot infer.
        let input = b"hello world";
        assert!(heuristic_size(input).is_none());
    }

    // ─── remap_coordinates ───────────────────────────────────

    #[test]
    fn remap_noop_when_offset_is_1_1() {
        // (1,1) offset must be a byte-identical no-op.
        let input = b"\x1b[5;10Hhello\x1b[1;24r";
        assert_eq!(remap_coordinates(input, 1, 1), input);
    }

    #[test]
    fn remap_cup_adds_offset() {
        // CUP \x1b[5;10H with row_offset=3, col_offset=5
        // -> \x1b[7;14H  (5+2, 10+4)
        let input = b"\x1b[5;10H";
        let out = remap_coordinates(input, 3, 5);
        assert_eq!(out, b"\x1b[7;14H");
    }

    #[test]
    fn remap_hvp_adds_offset() {
        // HVP \x1b[3;1f with row_offset=2, col_offset=4 -> \x1b[4;4f
        let input = b"\x1b[3;1f";
        let out = remap_coordinates(input, 2, 4);
        assert_eq!(out, b"\x1b[4;4f");
    }

    #[test]
    fn remap_cup_bare_home_defaults_to_1_1() {
        // \x1b[H means row=1,col=1; with offset (3,5) -> \x1b[3;5H
        let input = b"\x1b[H";
        let out = remap_coordinates(input, 3, 5);
        assert_eq!(out, b"\x1b[3;5H");
    }

    #[test]
    fn remap_scroll_region_adjusts_rows() {
        // \x1b[1;24r with row_offset=3 -> \x1b[3;26r
        let input = b"\x1b[1;24r";
        let out = remap_coordinates(input, 3, 1);
        assert_eq!(out, b"\x1b[3;26r");
    }

    #[test]
    fn remap_sgr_and_text_unchanged() {
        // SGR and plain text must pass through verbatim.
        let input = b"\x1b[38;5;130mhello\x1b[0m";
        let out = remap_coordinates(input, 5, 10);
        assert_eq!(out, input);
    }

    #[test]
    fn remap_relative_cursor_unchanged() {
        // Relative moves (A/B/C/D) must not be modified.
        let input = b"\x1b[3A\x1b[9C";
        let out = remap_coordinates(input, 5, 10);
        assert_eq!(out, input);
    }

    #[test]
    fn remap_mixed_stream() {
        // Realistic vim-like stream: CUP + SGR + text + scroll region.
        // row_offset=4, col_offset=5:
        //   \x1b[1;1H  -> \x1b[4;5H
        //   \x1b[24;1H -> \x1b[27;5H
        //   \x1b[1;24r -> \x1b[4;27r
        let input = b"\x1b[1;1H\x1b[38;5;130mline1\x1b[0m\x1b[24;1H\x1b[1;24r";
        let out = remap_coordinates(input, 4, 5);
        assert_eq!(
            out,
            b"\x1b[4;5H\x1b[38;5;130mline1\x1b[0m\x1b[27;5H\x1b[4;27r"
        );
    }

    // ─── write_header / record ────────────────────────────────────

    /// Header must be the first line of the recording file, with dir=="meta"
    /// and the correct rows/cols/program values.
    #[test]
    fn header_is_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("header_test.jsonl");

        let mut rec = Recording::new(path.clone()).unwrap();
        rec.write_header(24, 80, "vim");
        rec.record_out(b"hello");
        rec.close();

        let f = File::open(&path).unwrap();
        let mut lines = BufReader::new(f).lines();

        // First line must be the header.
        let first = lines.next().unwrap().unwrap();
        let header: RecordingHeader = serde_json::from_str(&first)
            .expect("first line must deserialize as RecordingHeader");
        assert_eq!(header.dir, "meta");
        assert_eq!(header.rows, 24);
        assert_eq!(header.cols, 80);
        assert_eq!(header.program, "vim");

        // Second line must be the data event.
        let second = lines.next().unwrap().unwrap();
        let event: RecordingEvent = serde_json::from_str(&second)
            .expect("second line must deserialize as RecordingEvent");
        assert_eq!(event.dir, "out");
    }

    #[test]
    fn record_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");

        let mut rec = Recording::new(path.clone()).unwrap();
        rec.record_out(b"hello");
        rec.record_in(b"ls\n");
        rec.record_out(b"file1\nfile2\n");
        rec.close();

        let f = File::open(&path).unwrap();
        let reader = BufReader::new(f);
        let events: Vec<RecordingEvent> = reader
            .lines()
            .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
            .collect();

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].dir, "out");
        assert_eq!(events[1].dir, "in");
        assert_eq!(events[2].dir, "out");

        let data0 = base64::engine::general_purpose::STANDARD
            .decode(&events[0].data)
            .unwrap();
        assert_eq!(data0, b"hello");
    }

    /// Regression test: when two out-events are 0–2 ms apart and the first one
    /// ends in a truncated escape sequence, timed_replay must merge them into
    /// a single write so the terminal never sees an incomplete sequence.
    ///
    /// Scenario mirrors the real bug:
    ///   event A (ts=0):   b"\x1b[38;5;130"   <- truncated 256-colour SGR
    ///   event B (ts=1):   b"m hello"          <- the missing 'm' terminator
    /// Without merging the terminal prints `m hello` as literal text.
    /// With merging the terminal receives `\x1b[38;5;130m hello` intact.
    #[test]
    fn timed_replay_merges_split_escape_sequence() {
        use tempfile::NamedTempFile;

        // Build a recording with two out-events 1 ms apart where the first
        // ends mid-escape-sequence.
        let mut tmp = NamedTempFile::new().unwrap();
        let base_ts: u64 = 1_000_000;

        // Event 0: truncated SGR (missing terminal 'm')
        let ev0 = RecordingEvent {
            ts: base_ts,
            dir: "out".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"\x1b[38;5;130"),
        };
        // Event 1: continuation arrives 1 ms later
        let ev1 = RecordingEvent {
            ts: base_ts + 1,
            dir: "out".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"m hello"),
        };
        // Event 2: a separate frame 200 ms later
        let ev2 = RecordingEvent {
            ts: base_ts + 200,
            dir: "out".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"world"),
        };

        for ev in &[&ev0, &ev1, &ev2] {
            writeln!(tmp, "{}", serde_json::to_string(ev).unwrap()).unwrap();
        }
        tmp.flush().unwrap();

        // Capture stdout of timed_replay using a pipe trick via a temp file.
        // Since timed_replay writes to stdout directly we test the merge logic
        // indirectly by inspecting the *event grouping* behaviour rather than
        // capturing real stdout (which would require process spawning).
        //
        // Instead, verify the invariant directly: after grouping, no group's
        // accumulated bytes end with a bare ESC or truncated CSI.
        let recording_path = tmp.path().to_path_buf();
        let f = File::open(&recording_path).unwrap();
        let reader = BufReader::new(f);

        let events: Vec<RecordingEvent> = reader
            .lines()
            .filter_map(|l| l.ok())
            .filter_map(|l| serde_json::from_str::<RecordingEvent>(&l).ok())
            .collect();

        // Simulate the merge logic from timed_replay.
        let mut groups: Vec<Vec<u8>> = Vec::new();
        let mut i = 0;
        while i < events.len() {
            let mut buf = Vec::<u8>::new();
            let mut j = i;
            loop {
                let ev = &events[j];
                if ev.dir == "out" {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&ev.data).unwrap();
                    buf.extend_from_slice(&bytes);
                }
                if j + 1 < events.len()
                    && events[j + 1].ts.saturating_sub(ev.ts) < MERGE_THRESHOLD_MS
                {
                    j += 1;
                } else {
                    break;
                }
            }
            if !buf.is_empty() { groups.push(buf); }
            i = j + 1;
        }

        // After merging: ev0 + ev1 form group 0, ev2 is group 1.
        assert_eq!(groups.len(), 2, "expected 2 merged groups, got {}", groups.len());

        // Group 0 must be the complete sequence, not split.
        assert_eq!(
            groups[0],
            b"\x1b[38;5;130m hello",
            "group 0 should be the merged complete escape sequence"
        );

        // Group 0 must NOT end with a truncated ESC sequence.
        let g0 = &groups[0];
        let last_esc = g0.iter().rposition(|&b| b == 0x1b);
        if let Some(pos) = last_esc {
            let tail = &g0[pos..];
            // A CSI sequence \x1b[ must be followed by at least one 0x40-0x7e byte.
            if tail.starts_with(b"\x1b[") {
                let has_terminator = tail[2..].iter().any(|&b| (0x40..=0x7e).contains(&b));
                assert!(has_terminator, "group 0 ends with incomplete CSI: {:?}", tail);
            }
        }

        // Group 1 is the separate frame.
        assert_eq!(groups[1], b"world");
    }

    // ─── filter_query_sequences: malformed DCS ────────────────────────────

    /// A DCS sequence without a String Terminator (\x1b\\) must NOT silently
    /// drop the bytes that follow it.  Before the fix the loop broke out of the
    /// while-loop entirely, discarding every subsequent byte.
    #[test]
    fn filter_malformed_dcs_does_not_drop_trailing_bytes() {
        // Malformed DCS (no ST), followed by a normal byte sequence.
        let input = b"before\x1bPno-terminator-hereafter";
        let out = filter_query_sequences(input);
        // "before" must survive; the ESC P prefix is skipped (2 bytes),
        // and then "no-terminator-hereafter" must follow.
        assert!(out.starts_with(b"before"), "'before' must be kept");
        assert!(
            out.ends_with(b"no-terminator-hereafter"),
            "bytes after malformed DCS must not be silently dropped: got {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    // ─── Recording::new: truncation on existing file ─────────────────────────

    /// If a recording file already exists (e.g. from a prior session that
    /// happened to share the same 8-char ID), opening it must TRUNCATE the old
    /// content so the header is always line 1.  The old append-mode behaviour
    /// would push the header onto line 2+, breaking replay.
    #[test]
    fn recording_truncates_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trunc_test.jsonl");

        // First recording: write some stale data.
        {
            let mut rec = Recording::new(path.clone()).unwrap();
            rec.write_header(24, 80, "bash");
            rec.record_out(b"stale data");
        }

        // Second recording with the same path: must overwrite, not append.
        {
            let mut rec = Recording::new(path.clone()).unwrap();
            rec.write_header(40, 120, "zsh");
            rec.record_out(b"fresh data");
        }

        let f = File::open(&path).unwrap();
        let lines: Vec<String> = BufReader::new(f)
            .lines()
            .map(|l| l.unwrap())
            .collect();

        // Must be exactly 2 lines: header + one event.
        assert_eq!(lines.len(), 2, "should have exactly 2 lines (header + data), got {}: {:?}", lines.len(), lines);

        let header: RecordingHeader = serde_json::from_str(&lines[0])
            .expect("first line must be a RecordingHeader");
        assert_eq!(header.dir, "meta", "first line must be meta header");
        assert_eq!(header.rows, 40, "should reflect the new session dimensions");
        assert_eq!(header.cols, 120);
        assert_eq!(header.program, "zsh");

        // The stale data must NOT appear anywhere.
        let all = lines.join("\n");
        assert!(!all.contains("stale"), "stale data must not appear in truncated file");
    }
}
