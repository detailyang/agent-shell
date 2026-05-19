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

fn timed_replay(reader: BufReader<File>, speed: f64) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let mut last_ts: Option<u64> = None;

    for line in reader.lines() {
        let line = line?;
        if let Ok(event) = serde_json::from_str::<RecordingEvent>(&line) {
            if let Some(prev) = last_ts {
                let delta_ms = event.ts.saturating_sub(prev) as f64 / speed;
                if delta_ms > 0.0 {
                    std::thread::sleep(std::time::Duration::from_millis(delta_ms as u64));
                }
            }
            last_ts = Some(event.ts);

            // Only replay output events (raw bytes preserve VT100 sequences)
            if event.dir == "out" {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&event.data)
                    .unwrap_or_default();
                out.write_all(&bytes)?;
                out.flush()?;
            }
            // Input events are silently skipped
        }
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
}
