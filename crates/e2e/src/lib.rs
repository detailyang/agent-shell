use agent_shell_core::protocol::{Request, Response};
use std::io::{Read, Write};
use std::process::{Child, Command, Output};
use std::time::Duration;

/// Handle to a running daemon process.
pub struct DaemonHandle {
    pub process: Child,
    socket_path: std::path::PathBuf,
    pub cli_bin: String,
}

impl DaemonHandle {
    /// Execute a CLI command and return the output.
    pub fn cli(&self, args: &[&str]) -> Output {
        Command::new(&self.cli_bin)
            .args(args)
            .output()
            .expect("failed to execute CLI")
    }

    /// Execute a CLI command and parse the response as JSON.
    pub fn cli_json(&self, args: &[&str]) -> Response {
        let output = self.cli(args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("failed to parse CLI output as JSON: {:?}\nOutput: {}", e, stdout)
        })
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
        AttachConnection::new(&self.socket_path, session_id, false)
    }

    /// Connect to the daemon's Unix socket for readonly attach testing.
    pub fn connect_attach_ro(
        &self,
        session_id: &str,
    ) -> Result<AttachConnection, String> {
        AttachConnection::new(&self.socket_path, session_id, true)
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Raw attach connection for testing bidirectional streaming.
pub struct AttachConnection {
    stream: std::os::unix::net::UnixStream,
}

impl AttachConnection {
    pub fn new(
        socket_path: &std::path::Path,
        session_id: &str,
        readonly: bool,
    ) -> Result<Self, String> {
        let mut stream = std::os::unix::net::UnixStream::connect(socket_path)
            .map_err(|e| format!("connect: {}", e))?;
        stream.set_nonblocking(false).ok();
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();

        // Send Attach request
        let req = Request::Attach {
            session_id: session_id.to_string(),
            readonly: if readonly { Some(true) } else { None },
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

        Ok(AttachConnection { stream })
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
pub fn start_daemon() -> DaemonHandle {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let base_dir = std::path::PathBuf::from(&home).join(".agent-shell");
    let socket_path = base_dir.join("daemon.sock");
    let pid_path = base_dir.join("daemon.pid");

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
    std::thread::sleep(Duration::from_millis(200));

    let daemon_bin = find_bin("agent-shell-daemon");
    let cli_bin = find_bin("agent-shell");

    let process = Command::new(&daemon_bin)
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

    assert!(socket_path.exists(), "daemon socket did not appear");

    DaemonHandle { process, socket_path, cli_bin }
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
