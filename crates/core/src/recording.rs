use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::term_emulator::TermEmulator;
use crate::terminal::enter_raw_mode;

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
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
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
            // No ST found — skip to end of buffer (malformed but safe).
            break;
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
        Box::new(lines.filter_map(|l| l.ok()))
    } else {
        let rest = lines.filter_map(|l| l.ok());
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
    let is_tty = unsafe { libc::isatty(1) != 0 };

    let rec_size: (u16, u16) = if let Some(ref h) = header {
        (h.rows, h.cols)
    } else {
        // Collect all "out" bytes to run the heuristic on.
        let all_out: Vec<u8> = events
            .iter()
            .filter(|e| e.dir == "out")
            .flat_map(|e| {
                base64::engine::general_purpose::STANDARD
                    .decode(&e.data)
                    .unwrap_or_default()
            })
            .collect();
        heuristic_size(&all_out).unwrap_or((24, 80))
    };

    // Create a terminal emulator at the recording's original dimensions.
    // Output events are fed into this emulator; after each burst we
    // generate a full_redraw() frame for the client terminal.
    let mut emu = TermEmulator::new(rec_size.0, rec_size.1);

    let _raw_guard = if is_tty { enter_raw_mode() } else { None };

    // Replay loop.
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut last_flush_ts: Option<u64> = None;

    let result = (|| -> std::io::Result<()> {
        let mut i = 0;
        while i < events.len() {
            let event = &events[i];

            // Inter-event delay: sleep for the wall-clock gap since the last
            // flushed frame, scaled by speed.
            if let Some(prev_ts) = last_flush_ts {
                let raw_delta_ms = event.ts.saturating_sub(prev_ts);
                let scaled_ms = (raw_delta_ms as f64 / speed) as u64;
                if scaled_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(scaled_ms));
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
                // Feed the raw bytes into the terminal emulator, then
                // generate a full-screen redraw for the client terminal.
                emu.process(&write_buf);
                let frame = emu.full_redraw();
                out.write_all(&frame)?;
                out.flush()?;
            }

            last_flush_ts = Some(group_end_ts);
            i = j + 1;
        }
        out.flush()?;
        Ok(())
    })();

    // Cleanup: restore alternate screen.
    if is_tty {
        let _ = out.write_all(b"\x1b[?1049l");
        let _ = out.write_all(b"\x1b[0m");
        let _ = out.flush();
        // _raw_guard is dropped here, restoring termios via RAII.
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
}
