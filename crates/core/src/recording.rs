use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::SystemTime;

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

fn timed_replay(reader: BufReader<File>, speed: f64) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // Pre-load all events so we can peek at the next one's timestamp.
    let events: Vec<RecordingEvent> = reader
        .lines()
        .filter_map(|l| l.ok())
        .filter_map(|l| serde_json::from_str::<RecordingEvent>(&l).ok())
        .collect();

    let mut last_flush_ts: Option<u64> = None;

    let mut i = 0;
    while i < events.len() {
        let event = &events[i];

        // ── Inter-event delay ───────────────────────────────────────────
        // Compute the wall-clock gap since the last flushed frame and sleep
        // for that duration (scaled by speed).  We use the timestamp of the
        // *first* event in the merged group as the reference point, so that
        // merging nearby events does not introduce extra latency.
        if let Some(prev_ts) = last_flush_ts {
            let raw_delta_ms = event.ts.saturating_sub(prev_ts);
            let scaled_ms = (raw_delta_ms as f64 / speed) as u64;
            if scaled_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(scaled_ms));
            }
        }

        // ── Merge micro-burst ────────────────────────────────────────────
        // Accumulate consecutive out-events that are within MERGE_THRESHOLD_MS
        // of each other into a single buffer, then flush once.
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

            // Peek at the next event: if it arrives within the merge window,
            // pull it into this group regardless of direction.
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
            out.write_all(&write_buf)?;
            out.flush()?;
        }

        last_flush_ts = Some(group_end_ts);
        i = j + 1;
    }

    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
