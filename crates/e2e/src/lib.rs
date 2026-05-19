use agent_shell_core::protocol::{Request, Response};
use std::io::{Read, Write};
use std::process::{Child, Command, Output};
use std::time::Duration;

/// Handle to a running daemon process.
pub struct DaemonHandle {
    pub process: Child,
    socket_path: std::path::PathBuf,
    pub cli_bin: String,
    _temp_dir: tempfile::TempDir, // Keep alive so directory isn't cleaned up prematurely
}

impl DaemonHandle {
    /// Execute a CLI command and return the output.
    pub fn cli(&self, args: &[&str]) -> Output {
        Command::new(&self.cli_bin)
            .args(args)
            .env("AGENT_SHELL_HOME", self.temp_dir_path())
            .output()
            .expect("failed to execute CLI")
    }

    /// Execute a CLI command and parse the response as JSON.
    /// Send a raw Request to the daemon over the Unix socket and return the Response.
    /// Used when the CLI argument format doesn't support the needed parameters
    /// (e.g. passing an explicit argv like `["vim", "/path/to/file"]`).
    pub fn rpc(&self, req: &Request) -> Response {
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(&self.socket_path)
            .expect("rpc: connect to daemon socket");
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
        let data = serde_json::to_vec(req).expect("rpc: serialize request");
        let len = data.len() as u32;
        stream.write_all(&len.to_be_bytes()).expect("rpc: write len");
        stream.write_all(&data).expect("rpc: write data");
        let mut lb = [0u8; 4];
        stream.read_exact(&mut lb).expect("rpc: read resp len");
        let rlen = u32::from_be_bytes(lb) as usize;
        let mut buf = vec![0u8; rlen];
        stream.read_exact(&mut buf).expect("rpc: read resp body");
        serde_json::from_slice(&buf).expect("rpc: parse response")
    }

    pub fn cli_json(&self, args: &[&str]) -> Response {
        let output = self.cli(args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("failed to parse CLI output as JSON: {:?}\nOutput: {}", e, stdout)
        })
    }

    /// Get the temp directory path for AGENT_SHELL_HOME
    pub fn temp_dir_path(&self) -> std::path::PathBuf {
        self._temp_dir.path().to_path_buf()
    }

    /// Stop the daemon.
    pub fn stop(&mut self) {
        let _ = self.cli(&["stop"]);
        let _ = self.process.wait();
    }

    /// Connect to the daemon's Unix socket for raw attach testing.
    pub fn connect_attach_rw(
        &self,
        session_id: &str,
    ) -> Result<AttachConnection, String> {
        AttachConnection::new(&self.socket_path, session_id, true)
    }

    /// Connect to the daemon's Unix socket for readonly attach testing (default mode).
    pub fn connect_attach_ro(
        &self,
        session_id: &str,
    ) -> Result<AttachConnection, String> {
        AttachConnection::new(&self.socket_path, session_id, false)
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        // Temp dir cleaned up by tempfile::TempDir drop
    }
}

/// Raw attach connection for testing bidirectional streaming.
pub struct AttachConnection {
    stream: std::os::unix::net::UnixStream,
    /// Raw PTY bytes from the initial handshake (base64-decoded).
    pub initial_output: Vec<u8>,
}

impl AttachConnection {
    pub fn new(
        socket_path: &std::path::Path,
        session_id: &str,
        writable: bool,
    ) -> Result<Self, String> {
        let mut stream = std::os::unix::net::UnixStream::connect(socket_path)
            .map_err(|e| format!("connect: {}", e))?;
        stream.set_nonblocking(false).ok();
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();

        // Send Attach request
        let req = Request::Attach {
            session_id: session_id.to_string(),
            writable: if writable { Some(true) } else { None },
        };
        let data = serde_json::to_vec(&req).map_err(|e| format!("serialize: {}", e))?;
        let len = data.len() as u32;
        stream.write_all(&len.to_be_bytes()).map_err(|e| format!("write: {}", e))?;
        stream.write_all(&data).map_err(|e| format!("write: {}", e))?;

        // Read initial JSON response
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).map_err(|e| format!("read len: {}", e))?;
        let resp_len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; resp_len];
        stream.read_exact(&mut buf).map_err(|e| format!("read resp: {}", e))?;
        let resp: Response = serde_json::from_slice(&buf).map_err(|e| format!("parse: {}", e))?;

        if !resp.ok {
            return Err(resp.error.unwrap_or_else(|| "attach failed".into()));
        }

        // Decode the base64-encoded initial output
        let initial_output = resp.output
            .as_ref()
            .and_then(|s| base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD, s
            ).ok())
            .unwrap_or_default();

        Ok(AttachConnection { stream, initial_output })
    }

    /// Send raw bytes to the PTY.
    pub fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.stream.write_all(data).map_err(|e| format!("send: {}", e))
    }

    /// Read available output (with timeout).
    pub fn read_output(&mut self, timeout: Duration) -> Vec<u8> {
        self.stream.set_read_timeout(Some(timeout)).ok();
        let mut output = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
        output
    }

    /// Send raw bytes and wait until any response bytes arrive (or timeout).
    /// Returns all bytes received within the timeout window.
    pub fn send_and_read(&mut self, data: &[u8], timeout: Duration) -> Vec<u8> {
        self.send(data).ok();
        // Small yield so the PTY has time to process the input.
        std::thread::sleep(Duration::from_millis(30));
        self.read_output(timeout)
    }

    /// Find the last CSI cursor-position sequence `ESC[row;colH` in a byte slice.
    /// Returns `(row, col)` (1-based) or `None`.
    pub fn last_cursor_pos(bytes: &[u8]) -> Option<(usize, usize)> {
        // Walk backwards looking for ESC [ ... H
        let mut i = bytes.len().saturating_sub(1);
        loop {
            if i + 2 >= bytes.len() {
                if i == 0 { return None; }
                i -= 1;
                continue;
            }
            if bytes[i] == b'\x1b' && bytes[i + 1] == b'[' {
                // scan for terminator
                let mut j = i + 2;
                while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'H' {
                    let inner = std::str::from_utf8(&bytes[i+2..j]).unwrap_or("");
                    let mut parts = inner.split(';');
                    let row: usize = parts.next().unwrap_or("1").parse().unwrap_or(1);
                    let col: usize = parts.next().unwrap_or("1").parse().unwrap_or(1);
                    return Some((row, col));
                }
            }
            if i == 0 { return None; }
            i -= 1;
        }
    }

    /// Collect output until `predicate` returns true or `timeout` elapses.
    /// Returns the full accumulated output.
    pub fn wait_for<F>(&mut self, timeout: Duration, predicate: F) -> Vec<u8>
    where F: Fn(&[u8]) -> bool
    {
        let deadline = std::time::Instant::now() + timeout;
        let mut all = Vec::new();
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let chunk = self.read_output(remaining.min(Duration::from_millis(50)));
            all.extend_from_slice(&chunk);
            if predicate(&all) { break; }
        }
        all
    }

    /// Send a command and wait for output containing the expected text.
    pub fn send_and_wait(
        &mut self,
        command: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        self.send(format!("{}\n", command).as_bytes())?;

        let deadline = std::time::Instant::now() + timeout;
        let mut all_output = Vec::new();

        while std::time::Instant::now() < deadline {
            let chunk = self.read_output(Duration::from_millis(200));
            all_output.extend_from_slice(&chunk);
            let text = String::from_utf8_lossy(&all_output);
            if text.contains(expected) {
                return Ok(text.to_string());
            }
        }

        let text = String::from_utf8_lossy(&all_output);
        Err(format!(
            "timeout waiting for '{}' in output. Got: {:?}",
            expected,
            &text[..text.len().min(500)]
        ))
    }
}

/// Start a daemon process for testing.
/// Each daemon gets its own isolated temp directory via AGENT_SHELL_HOME,
/// so parallel tests don't conflict.
pub fn start_daemon() -> DaemonHandle {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let base_dir = temp_dir.path().to_path_buf();
    let socket_path = base_dir.join("daemon.sock");
    let pid_path = base_dir.join("daemon.pid");

    // Ensure directory exists
    std::fs::create_dir_all(&base_dir).ok();

    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
    if pid_path.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                unsafe { libc::kill(pid, libc::SIGTERM); }
            }
        }
        let _ = std::fs::remove_file(&pid_path);
    }
    std::thread::sleep(Duration::from_millis(100));

    let daemon_bin = find_bin("agent-shell-daemon");
    let cli_bin = find_bin("agent-shell");

    let process = Command::new(&daemon_bin)
        .env("AGENT_SHELL_HOME", &base_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())  // Capture stderr for debugging
        .spawn()
        .expect("failed to start daemon");

    let mut retries = 20;
    while retries > 0 {
        if socket_path.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        retries -= 1;
    }

    assert!(socket_path.exists(), "daemon socket did not appear at {:?}", socket_path);

    DaemonHandle { process, socket_path, cli_bin, _temp_dir: temp_dir }
}

pub fn find_bin(name: &str) -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join(name);
            if path.exists() {
                return path.to_string_lossy().to_string();
            }
        }
    }
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap().parent().unwrap()
        .join("target/release").join(name);
    if path.exists() {
        return path.to_string_lossy().to_string();
    }
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap().parent().unwrap()
        .join("target/debug").join(name);
    if path.exists() {
        return path.to_string_lossy().to_string();
    }
    name.to_string()
}

/// Assert that a response is ok.
pub fn assert_ok(resp: &Response) {
    assert!(resp.ok, "expected ok: true, got: {:?}", resp);
}

/// Assert that a response is an error.
pub fn assert_error(resp: &Response, expected_error: &str) {
    assert!(!resp.ok, "expected ok: false, got: {:?}", resp);
    if let Some(ref err) = resp.error {
        assert!(err.contains(expected_error), "expected error containing '{}', got '{}'", expected_error, err);
    }
}

/// Extract session_id from a response.
pub fn session_id(resp: &Response) -> String {
    resp.session_id.clone().expect("expected session_id in response")
}

// ─── Render test helpers ──────────────────────────────────────────────

/// Which data path to test.
#[derive(Debug, Clone, Copy)]
pub enum RenderPath {
    /// Attach raw stream — validates raw bytes contain full escape sequences.
    Attach,
    /// `read` command — validates output field contains full escape sequences.
    ReadRaw,
    /// `read --screen` — validates VteGrid parsed text + cursor position.
    ReadScreen,
}

/// Send a printf command via CLI, then read output through the given path
/// and assert it contains the expected escape sequence and text.
pub fn assert_render(
    daemon: &DaemonHandle,
    sid: &str,
    printf_cmd: &str,
    path: RenderPath,
    expected_escape: &[u8],
    expected_text: &str,
) {
    // Send the command first
    let resp = daemon.cli_json(&[
        "send", "--session", sid, "--timeout", "5000", printf_cmd,
    ]);
    assert_ok(&resp);

    match path {
        RenderPath::Attach => {
            let mut conn = daemon.connect_attach_rw(sid).expect("attach connect");
            let mut bytes = conn.initial_output.clone();
            let stream = conn.read_output(std::time::Duration::from_millis(500));
            bytes.extend_from_slice(&stream);
            assert_contains_escape(&bytes, expected_escape, "attach render");
            assert_contains_text(&bytes, expected_text, "attach render");
        }
        RenderPath::ReadRaw => {
            let resp = daemon.cli_json(&["read", "--session", sid]);
            assert_ok(&resp);
            let output = resp.output.unwrap_or_default();
            // read output is UTF-8 lossy converted; escape sequences are still present as bytes
            let bytes = output.as_bytes();
            assert_contains_escape(bytes, expected_escape, "read render");
            assert_contains_text(bytes, expected_text, "read render");
        }
        RenderPath::ReadScreen => {
            let resp = daemon.cli_json(&["read", "--session", sid, "--screen"]);
            assert_ok(&resp);
            let screen = resp.screen.expect("expected screen data");
            let text = screen.join("\n");
            assert!(
                text.contains(expected_text),
                "screen render: expected text '{}' not found. Screen: {:?}",
                expected_text, &text[..text.len().min(500)]
            );
        }
    }
}

/// Assert raw bytes contain the given escape sequence.
pub fn assert_contains_escape(bytes: &[u8], needle: &[u8], label: &str) {
    assert!(
        bytes.windows(needle.len()).any(|w| w == needle),
        "{}: expected byte sequence {:?} not found.\n\
         Tail hex: {}\n\
         Tail text: {:?}",
        label, needle,
        hex_dump(&bytes[bytes.len().saturating_sub(80)..]),
        String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(200)..]),
    );
}

/// Assert raw bytes contain the given text.
pub fn assert_contains_text(bytes: &[u8], text: &str, label: &str) {
    let output = String::from_utf8_lossy(bytes);
    assert!(
        output.contains(text),
        "{}: expected text '{}' not found. Output tail: {:?}",
        label, text, &output[..output.len().min(500)],
    );
}

/// Assert bytes do not end with a truncated escape sequence.
/// A truncated escape is a bare `\x1b` at the end without a following `[` or `]`.
pub fn assert_no_truncated_escape(bytes: &[u8], label: &str) {
    if bytes.len() >= 1 && bytes[bytes.len() - 1] == 0x1b {
        panic!(
            "{}: output ends with bare ESC (truncated escape sequence).\n\
             Tail hex: {}\n\
             Tail text: {:?}",
            label,
            hex_dump(&bytes[bytes.len().saturating_sub(40)..]),
            String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(100)..]),
        );
    }
    // Also check if ESC is second-to-last with incomplete sequence
    if bytes.len() >= 2 {
        let last_two = &bytes[bytes.len() - 2..];
        if last_two[0] == 0x1b && last_two[1] != b'[' && last_two[1] != b']' && last_two[1] != b'O' {
            // Could be a valid 2-byte ESC sequence (ESC + letter), but not CSI/OSC
            // This is a soft warning — only panic for truly truncated CSI/OSC
            // ESC followed by a letter (like ESC M) is valid, so we don't panic here.
        }
    }
    // Check for truncated CSI: ESC [ without the final letter
    if bytes.len() >= 2 && bytes[bytes.len() - 2] == 0x1b && bytes[bytes.len() - 1] == b'[' {
        panic!(
            "{}: output ends with ESC [ (truncated CSI sequence).\n\
             Tail hex: {}",
            label,
            hex_dump(&bytes[bytes.len().saturating_sub(40)..]),
        );
    }
}

/// Hex dump of bytes.
pub fn hex_dump(data: &[u8]) -> String {
    data.iter().take(200).map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ")
}
