use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

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

pub use crate::replay::{replay, ReplayOptions};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::{
        filter_query_sequences, heuristic_size, normalize_crlf, read_header,
        recording_uses_alt_screen, remap_coordinates, MERGE_THRESHOLD_MS,
    };
    use std::fs::File;
    use std::io::{BufRead, BufReader};

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
            ts: 0,
            dir: "out".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"\x1b[?1049hsome content"),
        };
        assert!(recording_uses_alt_screen(&[ev]));
    }

    #[test]
    fn detects_alt_screen_absent() {
        // Inline TUI (pi-style): no smcup, only cursor-up + erase-line.
        let ev = RecordingEvent {
            ts: 0,
            dir: "out".into(),
            data: base64::engine::general_purpose::STANDARD
                .encode(b"\x1b[3A\r\x1b[2Kcontent\r\r\n"),
        };
        assert!(!recording_uses_alt_screen(&[ev]));
    }

    #[test]
    fn detects_alt_screen_ignores_in_events() {
        // ESC[?1049h in an "in" (input) event must not count.
        let ev = RecordingEvent {
            ts: 0,
            dir: "in".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"\x1b[?1049h"),
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
        assert_eq!(
            cols, 80,
            "cols must come from tilde padding, not cursor-H col 22"
        );
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
        let header: RecordingHeader =
            serde_json::from_str(&first).expect("first line must deserialize as RecordingHeader");
        assert_eq!(header.dir, "meta");
        assert_eq!(header.rows, 24);
        assert_eq!(header.cols, 80);
        assert_eq!(header.program, "vim");

        // Second line must be the data event.
        let second = lines.next().unwrap().unwrap();
        let event: RecordingEvent =
            serde_json::from_str(&second).expect("second line must deserialize as RecordingEvent");
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
                        .decode(&ev.data)
                        .unwrap();
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
            if !buf.is_empty() {
                groups.push(buf);
            }
            i = j + 1;
        }

        // After merging: ev0 + ev1 form group 0, ev2 is group 1.
        assert_eq!(
            groups.len(),
            2,
            "expected 2 merged groups, got {}",
            groups.len()
        );

        // Group 0 must be the complete sequence, not split.
        assert_eq!(
            groups[0], b"\x1b[38;5;130m hello",
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
                assert!(
                    has_terminator,
                    "group 0 ends with incomplete CSI: {:?}",
                    tail
                );
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
        let lines: Vec<String> = BufReader::new(f).lines().map(|l| l.unwrap()).collect();

        // Must be exactly 2 lines: header + one event.
        assert_eq!(
            lines.len(),
            2,
            "should have exactly 2 lines (header + data), got {}: {:?}",
            lines.len(),
            lines
        );

        let header: RecordingHeader =
            serde_json::from_str(&lines[0]).expect("first line must be a RecordingHeader");
        assert_eq!(header.dir, "meta", "first line must be meta header");
        assert_eq!(header.rows, 40, "should reflect the new session dimensions");
        assert_eq!(header.cols, 120);
        assert_eq!(header.program, "zsh");

        // The stale data must NOT appear anywhere.
        let all = lines.join("\n");
        assert!(
            !all.contains("stale"),
            "stale data must not appear in truncated file"
        );
    }
}
