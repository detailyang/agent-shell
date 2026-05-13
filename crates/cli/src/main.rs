use agent_shell_core::config::Config;
use agent_shell_core::protocol::{Request, Response};
use clap::{Parser, Subcommand};
use std::io::{IsTerminal, Write};
use std::os::unix::io::FromRawFd;
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
    Attach {
        #[arg(long, short)]
        session: String,
        #[arg(long)]
        readonly: bool,
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
        Commands::Attach { session, readonly } => {
            let config = Config::load();
            let socket_path = config.socket_path();
            let req = Request::Attach {
                session_id: session,
                readonly: if readonly { Some(true) } else { None },
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
                    println!("{{\"ok\":false,\"error\":\"{}\"}}", e.replace('"', "\\\""));
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
        Commands::Attach { session, readonly } => Request::Attach {
            session_id: session,
            readonly: if readonly { Some(true) } else { None },
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

// ─── Attach: bidirectional raw streaming ─────────────────────────────

/// RAII guard that restores the original terminal settings on drop.
struct RawModeGuard {
    original: nix::sys::termios::Termios,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let stdin_fd = unsafe { std::os::unix::io::OwnedFd::from_raw_fd(0) };
        let _ = nix::sys::termios::tcsetattr(
            &stdin_fd,
            nix::sys::termios::SetArg::TCSADRAIN,
            &self.original,
        );
        std::mem::forget(stdin_fd); // don't close fd 0
    }
}

fn enter_raw_mode() -> Option<RawModeGuard> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    // Borrow fd 0 without closing it
    let stdin_fd = unsafe { std::os::unix::io::OwnedFd::from_raw_fd(0) };
    let original = nix::sys::termios::tcgetattr(&stdin_fd).ok()?;
    let mut raw = original.clone();
    nix::sys::termios::cfmakeraw(&mut raw);
    nix::sys::termios::tcsetattr(&stdin_fd, nix::sys::termios::SetArg::TCSANOW, &raw).ok()?;
    std::mem::forget(stdin_fd);
    Some(RawModeGuard { original })
}

async fn run_attach(socket_path: &PathBuf, req: Request) -> Result<(), String> {
    let readonly = matches!(&req, Request::Attach { readonly: Some(true), .. });

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

    // Print the initial screen (VT100 full redraw)
    if let Some(output) = &resp.output {
        let _ = std::io::stdout().write_all(output.as_bytes());
        let _ = std::io::stdout().flush();
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
            // readonly: only read from daemon → stdout
            match stream_rx.read(&mut socket_buf).await {
                Ok(0) => running = false,
                Ok(n) => {
                    let _ = std::io::stdout().write_all(&socket_buf[..n]);
                    let _ = std::io::stdout().flush();
                }
                Err(_) => running = false,
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
