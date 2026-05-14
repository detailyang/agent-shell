use agent_shell_core::config::Config;
use agent_shell_core::protocol::{Request, Response};
use clap::{Parser, Subcommand};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Parser)]
#[command(name = "agent-shell", version, about = "AI agent PTY session manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Clone)]
enum Commands {
    /// Create a new PTY session
    Create {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        shell: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long = "env")]
        envs: Option<Vec<String>>,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        rows: Option<u16>,
        #[arg(long)]
        cols: Option<u16>,
        #[arg(long = "buffer-size")]
        buffer_size: Option<usize>,
        #[arg(long)]
        record: bool,
    },
    /// Destroy a session
    Destroy {
        #[arg(long, short)]
        session: String,
    },
    /// Send text or control character to a session
    Send {
        #[arg(long, short)]
        session: String,
        text: Option<String>,
        #[arg(long)]
        ctrl: Option<String>,
        #[arg(long)]
        nowait: bool,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long = "client-id")]
        client_id: Option<String>,
    },
    /// Read output from a session
    Read {
        #[arg(long, short)]
        session: String,
        #[arg(long)]
        screen: bool,
        #[arg(long = "client-id")]
        client_id: Option<String>,
    },
    /// Wait for a pattern in session output
    Wait {
        #[arg(long, short)]
        session: String,
        pattern: String,
        #[arg(long)]
        fixed: bool,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long = "client-id")]
        client_id: Option<String>,
    },
    /// Set or clear the prompt regex for a session
    SetPrompt {
        #[arg(long, short)]
        session: String,
        prompt: Option<String>,
    },
    /// List active sessions
    List,
    /// Attach to a session (interactive streaming I/O)
    /// If no session specified, shows an interactive picker.
    Attach {
        #[arg(long, short)]
        session: Option<String>,
        #[arg(long, short = 'W')]
        writable: bool,
    },
    /// Resize a session terminal
    Resize {
        #[arg(long, short)]
        session: String,
        #[arg(long)]
        rows: u16,
        #[arg(long)]
        cols: u16,
    },
    /// Replay a recording file
    Replay {
        file: PathBuf,
        #[arg(long, default_value = "1.0")]
        speed: f64,
        #[arg(long)]
        dump: bool,
        #[arg(long)]
        force: bool,
    },
    /// Stop the daemon gracefully (via socket)
    Stop,
    /// Force-kill the daemon process (via PID file + SIGKILL)
    KillDaemon,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Replay { file, speed, dump, force } => {
            let opts = agent_shell_core::recording::ReplayOptions { speed, dump, force };
            if let Err(e) = agent_shell_core::recording::replay(file, opts) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Attach { session, writable } => {
            let config = Config::load();
            let socket_path = config.socket_path();

            // Resolve session: if not specified, show interactive picker
            let session_id = match session {
                Some(id) => id,
                None => {
                    match pick_session(&socket_path).await {
                        Ok(Some(id)) => id,
                        Ok(None) => return, // user cancelled
                        Err(e) => {
                            eprintln!("Error: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
            };

            let req = Request::Attach {
                session_id,
                writable: if writable { Some(true) } else { None },
            };
            match run_attach(&socket_path, req).await {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::KillDaemon => {
            let config = Config::load();
            match kill_daemon(&config) {
                Ok(killed) => {
                    if killed {
                        println!("{{\"ok\":true,\"killed\":true}}");
                    } else {
                        println!("{{\"ok\":true,\"killed\":false,\"message\":\"no daemon running\"}}");
                    }
                }
                Err(e) => {
                    let err_json = serde_json::to_string(&e).unwrap_or_else(|_| "unknown error".to_string());
                    println!("{{\"ok\":false,\"error\":{}}}", err_json);
                    std::process::exit(1);
                }
            }
        }
        other => {
            let config = Config::load();
            let socket_path = config.socket_path();

            let resp = match connect_and_send(&socket_path, &other).await {
                Ok(r) => r,
                Err(e) => {
                    let e_lower = e.to_lowercase();
                    if e_lower.contains("connection refused") || e_lower.contains("no such file") {
                        if let Err(start_err) = auto_start_daemon(&socket_path).await {
                            eprintln!("Error: failed to start daemon: {}", start_err);
                            std::process::exit(1);
                        }
                        match connect_and_send(&socket_path, &other).await {
                            Ok(r) => r,
                            Err(e2) => {
                                eprintln!("Error: {}", e2);
                                std::process::exit(1);
                            }
                        }
                    } else {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                }
            };

            println!("{}", serde_json::to_string(&resp).unwrap());
            if !resp.ok {
                std::process::exit(1);
            }
        }
    }
}

fn command_to_request(cmd: Commands) -> Request {
    match cmd {
        Commands::Create { name, shell, cwd, envs, prompt, rows, cols, buffer_size, record } => {
            let env = envs.map(|pairs| {
                let mut map = std::collections::HashMap::new();
                for pair in pairs {
                    if let Some((k, v)) = pair.split_once('=') {
                        map.insert(k.to_string(), v.to_string());
                    }
                }
                map
            });
            Request::Create { name, shell, cwd, env, prompt, rows, cols, buffer_size, record: if record { Some(true) } else { None } }
        }
        Commands::Destroy { session } => Request::Destroy { session_id: session },
        Commands::Send { session, text, ctrl, nowait, timeout, client_id } => Request::Send {
            session_id: session,
            text: text.unwrap_or_default(),
            ctrl,
            nowait: if nowait { Some(true) } else { None },
            timeout_ms: timeout,
            client_id,
        },
        Commands::Read { session, screen, client_id } => Request::Read {
            session_id: session,
            screen: if screen { Some(true) } else { None },
            client_id,
        },
        Commands::Wait { session, pattern, fixed, timeout, client_id } => Request::Wait {
            session_id: session,
            pattern,
            fixed: if fixed { Some(true) } else { None },
            timeout_ms: timeout,
            client_id,
        },
        Commands::SetPrompt { session, prompt } => Request::SetPrompt { session_id: session, prompt },
        Commands::List => Request::List,
        Commands::Attach { session, writable } => Request::Attach {
            session_id: session.unwrap_or_default(),
            writable: if writable { Some(true) } else { None },
        },
        Commands::Resize { session, rows, cols } => Request::Resize { session_id: session, rows, cols },
        Commands::Stop => Request::Stop,
        Commands::KillDaemon => Request::Stop, // handled locally, never sent
        Commands::Replay { .. } => unreachable!(),
    }
}

// ─── Normal (non-streaming) command ──────────────────────────────────

async fn connect_and_send(socket_path: &PathBuf, cmd: &Commands) -> Result<Response, String> {
    let req = command_to_request(cmd.clone());

    let mut stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .map_err(|e| format!("connect: {}", e))?;

    let data = serde_json::to_vec(&req).map_err(|e| format!("serialize: {}", e))?;
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes()).await.map_err(|e| format!("write: {}", e))?;
    stream.write_all(&data).await.map_err(|e| format!("write: {}", e))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(|e| format!("read: {}", e))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.map_err(|e| format!("read: {}", e))?;

    serde_json::from_slice(&buf).map_err(|e| format!("deserialize: {}", e))
}

// ─── Session picker TUI ──────────────────────────────────────────────

/// Fetch session list from the daemon. Auto-starts daemon if needed.
async fn fetch_sessions(socket_path: &PathBuf) -> Result<Vec<agent_shell_core::protocol::SessionInfo>, String> {
    let req = Request::List;
    let mut stream = match tokio::net::UnixStream::connect(socket_path).await {
        Ok(s) => s,
        Err(e) => {
            let e_lower = e.to_string().to_lowercase();
            if e_lower.contains("connection refused") || e_lower.contains("no such file") {
                auto_start_daemon(socket_path).await?;
                tokio::net::UnixStream::connect(socket_path)
                    .await
                    .map_err(|e| format!("connect after auto-start: {}", e))?
            } else {
                return Err(format!("connect: {}", e));
            }
        }
    };

    let data = serde_json::to_vec(&req).map_err(|e| format!("serialize: {}", e))?;
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes()).await.map_err(|e| format!("write: {}", e))?;
    stream.write_all(&data).await.map_err(|e| format!("write: {}", e))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(|e| format!("read: {}", e))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.map_err(|e| format!("read: {}", e))?;

    let resp: Response = serde_json::from_slice(&buf).map_err(|e| format!("deserialize: {}", e))?;
    if !resp.ok {
        return Err(resp.error.unwrap_or_else(|| "list failed".into()));
    }
    Ok(resp.sessions.unwrap_or_default())
}

/// Show an interactive session picker and return the selected session ID.
/// Returns Ok(None) if the user cancels (Esc/Ctrl-C) or there are no sessions.
async fn pick_session(socket_path: &PathBuf) -> Result<Option<String>, String> {
    let sessions = fetch_sessions(socket_path).await?;

    // Filter to active (non-exited) sessions
    let active: Vec<_> = sessions.into_iter().filter(|s| !s.exited).collect();
    if active.is_empty() {
        eprintln!("No active sessions. Create one with: agent-shell create");
        return Ok(None);
    }

    // If only one session, auto-select it
    // REMOVED: always show the picker so user has control
    // if active.len() == 1 {
    //     let s = &active[0];
    //     let label = session_label(s);
    //     eprintln!("Only one session, auto-selecting: {}", label);
    //     return Ok(Some(s.id.clone()));
    // }

    // Interactive picker — requires a terminal
    if !std::io::stdin().is_terminal() {
        eprintln!("{} session(s) available but stdin is not a terminal.", active.len());
        eprintln!("Specify a session with: agent-shell attach --session <ID>");
        eprintln!("\nActive sessions:");
        for s in &active {
            eprintln!("  {}  {}", s.id, session_label(s));
        }
        return Ok(None);
    }

    // Enter raw mode for interactive selection
    let raw_guard = enter_raw_mode().ok_or("failed to enter raw mode")?;
    let result = run_picker(&active);
    drop(raw_guard); // restore terminal before any further output

    result
}

/// Format a session label for display.
fn session_label(s: &agent_shell_core::protocol::SessionInfo) -> String {
    let name = s.name.as_deref().unwrap_or("<unnamed>");
    format!("{} ({}, pid {})", s.id, name, s.pid)
}

/// Run the interactive picker. Terminal must already be in raw mode.
/// Returns Ok(Some(session_id)) on selection, Ok(None) on cancel.
fn run_picker(sessions: &[agent_shell_core::protocol::SessionInfo]) -> Result<Option<String>, String> {
    let mut selected = 0usize;
    let count = sessions.len();
    let stdout = std::io::stdout();
    let mut stdout_lock = stdout.lock();
    let mut first_draw = true;

    // Main event loop — read raw keypresses
    let stdin_fd = 0i32; // stdin
    let mut buf = [0u8; 16];
    loop {
        // Draw current state
        draw_picker(&mut stdout_lock, sessions, selected, first_draw)?;
        first_draw = false;

        // Wait for input
        let mut pfd = [nix::poll::PollFd::new(
            unsafe { std::os::unix::io::BorrowedFd::borrow_raw(stdin_fd) },
            nix::poll::PollFlags::POLLIN,
        )];
        match nix::poll::poll(&mut pfd, nix::poll::PollTimeout::try_from(5000).unwrap()) {
            Ok(0) => continue, // timeout, redraw
            Ok(_) => {}
            Err(e) => return Err(format!("poll: {}", e)),
        }

        // Read raw bytes from stdin
        let n = match nix::unistd::read(stdin_fd, &mut buf) {
            Ok(n) => n,
            Err(e) => return Err(format!("read stdin: {}", e)),
        };

        let key = &buf[..n];

        if key == b"\x1b" || key == b"\x03" {
            // Esc or Ctrl-C → cancel
            clear_picker(&mut stdout_lock, sessions.len())?;
            return Ok(None);
        } else if key == b"\r" || key == b"\n" {
            // Enter → select
            clear_picker(&mut stdout_lock, sessions.len())?;
            return Ok(Some(sessions[selected].id.clone()));
        } else if key == b"\x1b[A" || key == b"k" {
            // Up arrow or k
            if selected > 0 { selected -= 1; }
        } else if key == b"\x1b[B" || key == b"j" {
            // Down arrow or j
            if selected + 1 < count { selected += 1; }
        } else if key == b"\x1b[1;2A" || key == b"K" {
            // Shift-Up or K → jump to top
            selected = 0;
        } else if key == b"\x1b[1;2B" || key == b"J" {
            // Shift-Down or J → jump to bottom
            selected = count - 1;
        } else if !key.is_empty() && key[0] >= b'1' && key[0] <= b'9' {
            // Number key → direct selection (1-based)
            let idx = (key[0] - b'1') as usize;
            if idx < count { selected = idx; }
        }
        // else: ignore unknown key
    }
}

/// Draw the picker UI.
/// On first draw, we print from the current cursor position.
/// On subsequent draws, we move cursor up to overwrite the previous output.
fn draw_picker(
    stdout: &mut dyn std::io::Write,
    sessions: &[agent_shell_core::protocol::SessionInfo],
    selected: usize,
    first_draw: bool,
) -> Result<(), String> {
    let line_count = sessions.len() + 1; // sessions + help line

    if !first_draw {
        // Move cursor up to the top of the picker area
        write!(stdout, "\x1b[{}A", line_count).map_err(|e| format!("write: {}", e))?;
    }

    // Draw session lines
    for (i, s) in sessions.iter().enumerate() {
        let name = s.name.as_deref().unwrap_or("<unnamed>");
        let indicator = if i == selected { "\u{276f}" } else { " " }; // ❯
        let highlight_on = if i == selected { "\x1b[1;36m" } else { "" };
        let highlight_off = if i == selected { "\x1b[0m" } else { "" };
        let num = i + 1;
        write!(
            stdout,
            "\x1b[2K\r{} {}{}{}. {} (pid {})\n",
            indicator, highlight_on, num, highlight_off, name, s.pid,
        )
        .map_err(|e| format!("write: {}", e))?;
    }

    // Draw help line
    write!(stdout, "\x1b[2K\r\x1b[2m\u{2191}\u{2193} select \u{00b7} Enter attach \u{00b7} Esc cancel\x1b[0m")
        .map_err(|e| format!("write: {}", e))?;

    stdout.flush().map_err(|e| format!("flush: {}", e))?;
    Ok(())
}

/// Clear the picker from the terminal.
fn clear_picker(stdout: &mut dyn std::io::Write, line_count: usize) -> Result<(), String> {
    // Move down to the bottom of the picker area
    write!(stdout, "\x1b[{}B", line_count).map_err(|e| format!("write: {}", e))?;
    // Clear each line moving up
    for _ in 0..=line_count {
        write!(stdout, "\x1b[2K\x1b[1A\r").map_err(|e| format!("write: {}", e))?;
    }
    // Clear the help line
    write!(stdout, "\x1b[2K\r").map_err(|e| format!("write: {}", e))?;
    stdout.flush().map_err(|e| format!("flush: {}", e))?;
    Ok(())
}

// ─── Attach: bidirectional raw streaming ─────────────────────────────

/// RAII guard that restores the original terminal settings on drop.
struct RawModeGuard {
    original: nix::sys::termios::Termios,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // SAFETY: fd 0 is stdin, always valid while process is alive.
        // BorrowedFd does not close on drop.
        let stdin_fd = unsafe { std::os::unix::io::BorrowedFd::borrow_raw(0) };
        let _ = nix::sys::termios::tcsetattr(
            &stdin_fd,
            nix::sys::termios::SetArg::TCSADRAIN,
            &self.original,
        );
    }
}

fn enter_raw_mode() -> Option<RawModeGuard> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    // SAFETY: fd 0 is stdin, always valid while process is alive.
    // BorrowedFd does not close on drop.
    let stdin_fd = unsafe { std::os::unix::io::BorrowedFd::borrow_raw(0) };
    let original = nix::sys::termios::tcgetattr(&stdin_fd).ok()?;
    let mut raw = original.clone();
    nix::sys::termios::cfmakeraw(&mut raw);
    nix::sys::termios::tcsetattr(&stdin_fd, nix::sys::termios::SetArg::TCSANOW, &raw).ok()?;
    Some(RawModeGuard { original })
}

async fn run_attach(socket_path: &PathBuf, req: Request) -> Result<(), String> {
    let readonly = !matches!(&req, Request::Attach { writable: Some(true), .. });

    // Auto-start daemon if needed
    let mut stream = match tokio::net::UnixStream::connect(socket_path).await {
        Ok(s) => s,
        Err(e) => {
            let e_lower = e.to_string().to_lowercase();
            if e_lower.contains("connection refused") || e_lower.contains("no such file") {
                auto_start_daemon(socket_path).await?;
                tokio::net::UnixStream::connect(socket_path)
                    .await
                    .map_err(|e| format!("connect after auto-start: {}", e))?
            } else {
                return Err(format!("connect: {}", e));
            }
        }
    };

    // ── Phase 1: send request & read JSON handshake ────────────────
    let data = serde_json::to_vec(&req).map_err(|e| format!("serialize: {}", e))?;
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes()).await.map_err(|e| format!("write: {}", e))?;
    stream.write_all(&data).await.map_err(|e| format!("write: {}", e))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(|e| format!("read handshake: {}", e))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.map_err(|e| format!("read handshake: {}", e))?;
    let resp: Response = serde_json::from_slice(&buf).map_err(|e| format!("parse handshake: {}", e))?;

    if !resp.ok {
        // Print error as JSON and exit
        println!("{}", serde_json::to_string(&resp).unwrap());
        return Err(resp.error.unwrap_or_else(|| "attach failed".into()));
    }

    // Decode the base64-encoded raw PTY output and write to terminal.
    // This preserves all escape sequences (colors, cursor movement, etc.).
    if let Some(output) = &resp.output {
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, output) {
            Ok(bytes) => {
                let _ = std::io::stdout().write_all(&bytes);
                let _ = std::io::stdout().flush();
            }
            Err(_) => {
                // Fallback: treat as plain text (for backwards compat)
                let _ = std::io::stdout().write_all(output.as_bytes());
                let _ = std::io::stdout().flush();
            }
        }
    }

    // ── Phase 2: raw binary bidirectional streaming ────────────────
    let _raw_guard = enter_raw_mode();

    let (mut stream_rx, mut stream_tx) = stream.into_split();
    let mut stdin_handle = tokio::io::stdin();
    let mut running = true;

    while running {
        let mut stdin_buf = [0u8; 4096];
        let mut socket_buf = [0u8; 4096];

        if readonly {
            // readonly (default): only read from daemon → stdout
            // Ctrl-D (0x04) exits, Ctrl-C (0x03) also exits
            tokio::select! {
                // daemon → stdout (raw PTY output)
                result = stream_rx.read(&mut socket_buf) => {
                    match result {
                        Ok(0) => running = false,
                        Ok(n) => {
                            let _ = std::io::stdout().write_all(&socket_buf[..n]);
                            let _ = std::io::stdout().flush();
                        }
                        Err(_) => running = false,
                    }
                }
                // stdin → check for exit keys only
                result = stdin_handle.read(&mut stdin_buf) => {
                    match result {
                        Ok(0) => running = false, // stdin EOF
                        Ok(n) => {
                            let data = &stdin_buf[..n];
                            // Ctrl-C or Ctrl-D exits readonly attach
                            if data.contains(&0x03) || data.contains(&0x04) {
                                running = false;
                            }
                            // All other input ignored in readonly mode
                        }
                        Err(_) => running = false,
                    }
                }
            }
        } else {
            tokio::select! {
                // daemon → stdout (raw PTY output)
                result = stream_rx.read(&mut socket_buf) => {
                    match result {
                        Ok(0) => running = false,
                        Ok(n) => {
                            let _ = std::io::stdout().write_all(&socket_buf[..n]);
                            let _ = std::io::stdout().flush();
                        }
                        Err(_) => running = false,
                    }
                }
                // stdin → daemon (keystroke forwarding)
                result = stdin_handle.read(&mut stdin_buf) => {
                    match result {
                        Ok(0) => running = false, // stdin EOF
                        Ok(n) => {
                            let data = &stdin_buf[..n];
                            // Ctrl-C (0x03) detaches without forwarding to PTY
                            if data.contains(&0x03) {
                                running = false;
                            } else if stream_tx.write_all(data).await.is_err() {
                                running = false;
                            }
                        }
                        Err(_) => running = false,
                    }
                }
            }
        }
    }

    // _raw_guard dropped here → terminal restored
    Ok(())
}

// ─── Daemon auto-start ───────────────────────────────────────────────

async fn auto_start_daemon(socket_path: &PathBuf) -> Result<(), String> {
    if socket_path.exists() {
        let _ = std::fs::remove_file(socket_path);
    }
    let daemon_bin = find_daemon_binary()?;
    std::process::Command::new(&daemon_bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn daemon: {}", e))?;

    let mut delay = 100u64;
    for _ in 0..5 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        if socket_path.exists() {
            if tokio::net::UnixStream::connect(socket_path).await.is_ok() {
                return Ok(());
            }
        }
        delay *= 2;
    }
    Err("daemon failed to start".into())
}

fn find_daemon_binary() -> Result<String, String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join("agent-shell-daemon");
            if path.exists() {
                return Ok(path.to_string_lossy().to_string());
            }
        }
    }
    if let Ok(output) = std::process::Command::new("which").arg("agent-shell-daemon").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(path);
            }
        }
    }
    Err("agent-shell-daemon not found in PATH or next to agent-shell".into())
}

// ─── Force-kill daemon (bypasses socket) ─────────────────────────────

/// Read PID file, send SIGKILL, clean up socket/pid artifacts.
/// Returns Ok(true) if a process was killed, Ok(false) if no daemon was running.
fn kill_daemon(config: &Config) -> Result<bool, String> {
    let base_dir = Config::base_dir();
    let pid_path = base_dir.join("daemon.pid");
    let socket_path = config.socket_path();

    // Read PID — if no pid file, there's nothing to kill
    let pid_str = match std::fs::read_to_string(&pid_path) {
        Ok(s) => s,
        Err(_) => {
            // Clean up any stale socket
            let _ = std::fs::remove_file(&socket_path);
            return Ok(false); // no daemon running
        }
    };
    let pid: i32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            let _ = std::fs::remove_file(&socket_path);
            let _ = std::fs::remove_file(&pid_path);
            return Ok(false); // corrupt pid file
        }
    };

    // Check if the process exists
    let exists = unsafe { libc::kill(pid, 0) == 0 };
    if !exists {
        // Stale artifacts — clean them up
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(&pid_path);
        return Ok(false);
    }

    // SIGKILL
    let ret = unsafe { libc::kill(pid, libc::SIGKILL) };
    if ret != 0 {
        return Err(format!(
            "failed to kill PID {}: {}",
            pid,
            std::io::Error::last_os_error()
        ));
    }

    // Wait briefly for the process to die, and reap zombie
    let mut waited = 0;
    while waited < 2000 {
        // Try to reap the zombie
        let _ = nix::sys::wait::waitpid(
            nix::unistd::Pid::from_raw(pid),
            Some(nix::sys::wait::WaitPidFlag::WNOHANG),
        );
        if unsafe { libc::kill(pid, 0) != 0 } {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 50;
    }

    // Clean up artifacts
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&pid_path);

    Ok(true)
}
