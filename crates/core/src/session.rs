use crate::config::Config;
use crate::recording::Recording;
use crate::ringbuf::RingBuffer;
use crate::vte_grid::VteGrid;
use nix::sys::signal::{self, Signal};
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use regex::Regex;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::io::BorrowedFd;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

/// Default send/wait timeout in milliseconds.
pub const DEFAULT_TIMEOUT_MS: u64 = 30000;

/// Minimum buffer size.
pub const MIN_BUFFER_SIZE: usize = 4096;

pub struct Session {
    pub id: String,
    pub name: Option<String>,
    pub pty_master: Box<dyn MasterPty + Send>,
    pub pty_reader: Box<dyn std::io::Read + Send>,
    pub pty_writer: std::sync::Mutex<Box<dyn std::io::Write + Send>>,
    pub ringbuf: RingBuffer,
    pub vte_grid: VteGrid,
    pub shell_pgid: i32,
    pub current_fg_pgid: i32,
    pub prompt_regex: Option<Regex>,
    pub child_pid: u32,
    pub exited: Option<i32>,
    pub created_at: u64,  // Unix timestamp in seconds
    pub created_at_instant: Instant,  // For elapsed time calculations
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub rows: u16,
    pub cols: u16,
    pub buffer_size: usize,
    pub shell: String,
    pub send_seq: u64,
    pub recording: Option<Recording>,
    pub prev_fg_pgid: i32,
    /// portable-pty Child handle — needed to properly reap zombies.
    child: Box<dyn Child + Send + Sync>,
    /// Separate killer handle — allows killing without &mut self (useful from
    /// background tasks or when child is already being waited on).
    child_killer: Box<dyn ChildKiller + Send + Sync>,
}

impl Session {
    /// Create a new PTY session.
    pub fn new(
        config: &Config,
        name: Option<String>,
        shell: Option<String>,
        cwd: Option<PathBuf>,
        env: Option<HashMap<String, String>>,
        prompt: Option<String>,
        rows: Option<u16>,
        cols: Option<u16>,
        buffer_size: Option<usize>,
        record: Option<bool>,
    ) -> Result<Self, String> {
        let shell = shell.unwrap_or_else(|| config.session.default_shell.clone());
        let rows = rows.unwrap_or(config.session.default_rows);
        let cols = cols.unwrap_or(config.session.default_cols);
        let buffer_size = buffer_size
            .unwrap_or(config.session.default_buffer_size)
            .max(MIN_BUFFER_SIZE);

        let prompt_regex = match prompt {
            Some(ref p) if !p.is_empty() => Some(
                Regex::new(p).map_err(|e| format!("invalid regex: {}", e))?,
            ),
            _ => None,
        };

        let id = uuid::Uuid::new_v4().to_string()[..8].to_string();

        let pty_system = native_pty_system();

        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("failed to open pty: {}", e))?;

        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(cwd.clone().unwrap_or_else(|| PathBuf::from(".")));

        if let Some(env) = env {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("failed to spawn: {}", e))?;

        let child_pid = child
            .process_id()
            .ok_or_else(|| "failed to get child pid".to_string())?;

        // Clone a killer handle before we move child into Session
        let child_killer = child.clone_killer();

        // Get the initial foreground process group
        // Retry up to 500ms because the shell may not have set its pgid yet
        let master_fd = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| "failed to get master fd".to_string())?;
        let mut shell_pgid: i32 = 0;
        for _ in 0..10 {
            // SAFETY: master_fd is a valid, owned fd inside pty_master.
            let borrowed_fd = unsafe { BorrowedFd::borrow_raw(master_fd) };
            if let Ok(pgid) = nix::unistd::tcgetpgrp(&borrowed_fd) {
                shell_pgid = pgid.as_raw();
                if shell_pgid != 0 {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if shell_pgid == 0 {
            // Fallback: use the child's pid as the shell pgid
            shell_pgid = child_pid as i32;
        }

        // Get reader and writer handles
        let pty_reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("failed to clone reader: {}", e))?;
        let pty_writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("failed to take writer: {}", e))?;

        // Create recording if requested
        let recording = if record.unwrap_or(config.session.record_by_default) {
            let rec_dir = config.recording_dir();
            let rec_path = rec_dir.join(format!("{}.jsonl", id));
            Recording::new(rec_path).ok()
        } else {
            None
        };

        drop(pair.slave); // Drop slave handle

        Ok(Session {
            id,
            name,
            pty_master: pair.master,
            pty_reader,
            pty_writer: std::sync::Mutex::new(pty_writer),
            ringbuf: RingBuffer::new(buffer_size),
            vte_grid: VteGrid::new(rows, cols),
            shell_pgid,
            current_fg_pgid: shell_pgid,
            prev_fg_pgid: shell_pgid,
            prompt_regex,
            child_pid,
            exited: None,
            created_at: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            created_at_instant: Instant::now(),
            cwd,
            env: HashMap::new(),
            rows,
            cols,
            buffer_size,
            shell,
            send_seq: 0,
            recording,
            child,
            child_killer,
        })
    }

    /// Read available output from PTY master and write to ringbuf + vte_grid.
    /// The PTY reader fd should be set to non-blocking for this to work properly.
    pub fn feed(&mut self) -> usize {
        let mut buf = [0u8; 65536];
        let mut total = 0;

        loop {
            match self.pty_reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = &buf[..n];
                    self.ringbuf.write(data);
                    self.vte_grid.process(data);
                    if let Some(ref mut rec) = self.recording {
                        rec.record_out(data);
                    }
                    total += n;
                    if n < buf.len() {
                        break;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(ref e) if e.raw_os_error() == Some(libc::EAGAIN) => break,
                Err(_) => break,
            }
        }

        total
    }

    /// Send text to the PTY (appends newline).
    pub fn send_text(&mut self, text: &str) -> Result<(), String> {
        let data = format!("{}\n", text);
        let mut writer = self.pty_writer.lock().unwrap();
        writer
            .write_all(data.as_bytes())
            .map_err(|e| format!("write failed: {}", e))?;
        writer
            .flush()
            .map_err(|e| format!("flush failed: {}", e))?;
        drop(writer);
        if let Some(ref mut rec) = self.recording {
            rec.record_in(data.as_bytes());
        }
        Ok(())
    }

    /// Send a control character to the PTY.
    pub fn send_ctrl(&mut self, ctrl: &str) -> Result<(), String> {
        let byte = match ctrl {
            "c" => 0x03,
            "d" => 0x04,
            "z" => 0x1a,
            _ => return Err(format!("unknown control char: {}", ctrl)),
        };
        let mut writer = self.pty_writer.lock().unwrap();
        writer
            .write_all(&[byte])
            .map_err(|e| format!("write failed: {}", e))?;
        writer
            .flush()
            .map_err(|e| format!("flush failed: {}", e))?;
        drop(writer);
        if let Some(ref mut rec) = self.recording {
            rec.record_in(&[byte]);
        }
        Ok(())
    }

    /// Check the current foreground process group ID.
    pub fn check_fg_pgid(&mut self) -> i32 {
        let master_fd = match self.pty_master.as_raw_fd() {
            Some(fd) => fd,
            None => return self.current_fg_pgid,
        };
        // SAFETY: master_fd is a valid, owned fd inside pty_master.
        // We borrow it for tcgetpgrp without taking ownership.
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(master_fd) };
        let result = nix::unistd::tcgetpgrp(&borrowed_fd)
            .map(|p| p.as_raw())
            .unwrap_or(self.current_fg_pgid);

        self.prev_fg_pgid = self.current_fg_pgid;
        self.current_fg_pgid = result;
        result
    }

    /// Check if the child process has exited (non-blocking).
    /// Uses portable-pty's `try_wait()` which calls `waitpid(WNOHANG)` internally,
    /// properly reaping the zombie if the child has exited.
    pub fn check_exited(&mut self) -> Option<i32> {
        if self.exited.is_some() {
            return self.exited;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let code = if status.success() {
                    0
                } else {
                    status.exit_code() as i32
                };
                self.exited = Some(code);
                self.close_recording();
                Some(code)
            }
            Ok(None) => None, // still running
            Err(_) => {
                // ECHILD means already reaped
                self.exited = Some(1);
                Some(1)
            }
        }
    }

    /// Allocate the next send sequence number.
    pub fn next_seq(&mut self) -> u64 {
        self.send_seq += 1;
        self.send_seq
    }

    /// Check if fg_pgid has returned to the shell pgid (from a different value).
    pub fn fg_returned_to_shell(&self) -> bool {
        self.current_fg_pgid == self.shell_pgid && self.prev_fg_pgid != self.shell_pgid
    }

    /// Kill the child process with SIGHUP.
    pub fn kill(&mut self) {
        let _ = signal::kill(
            nix::unistd::Pid::from_raw(self.child_pid as i32),
            Signal::SIGHUP,
        );
    }

    /// Force kill the child process with SIGKILL, then reap the zombie.
    pub fn force_kill(&mut self) {
        let _ = self.child_killer.kill();
        // Reap zombie immediately
        let _ = self.child.try_wait();
    }

    /// Resize the PTY.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.pty_master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("resize failed: {}", e))?;
        self.vte_grid.resize(rows, cols);
        self.rows = rows;
        self.cols = cols;
        Ok(())
    }

    /// Close the recording file if active.
    pub fn close_recording(&mut self) {
        if let Some(ref mut rec) = self.recording {
            rec.close();
        }
        self.recording = None;
    }

    /// Get the raw master fd for async operations.
    pub fn master_fd(&self) -> Option<std::os::unix::io::RawFd> {
        self.pty_master.as_raw_fd()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Ensure the child is reaped even if nobody called check_exited().
        self.close_recording();
        let _ = self.child.try_wait();
    }
}
