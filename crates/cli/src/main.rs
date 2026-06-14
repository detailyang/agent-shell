mod server;

use agent_shell_core::config::Config;
use agent_shell_core::protocol::{Request, Response};
use agent_shell_core::terminal::{enter_raw_mode, terminal_size};
use clap::{Parser, Subcommand};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::watch;

/// Global flag set by the signal handler (daemon mode only).
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn sig_shutdown_handler(_sig: nix::libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

fn install_signal_handlers() {
    let handler = nix::sys::signal::SigHandler::Handler(sig_shutdown_handler);
    let action = nix::sys::signal::SigAction::new(
        handler,
        nix::sys::signal::SaFlags::SA_RESTART,
        nix::sys::signal::SigSet::empty(),
    );
    unsafe {
        let _ = nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGTERM, &action);
        let _ = nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGINT, &action);
    }
}

#[derive(Parser)]
#[command(name = "agent-shell", version, about = "AI agent PTY session manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Clone)]
enum Commands {
    /// Create a new PTY session.
    ///
    /// Examples:
    ///   agent-shell create                          # start default shell
    ///   agent-shell create --shell /bin/zsh          # start zsh
    Create {
        #[arg(long)]
        name: Option<String>,
        /// Executable to launch (e.g. /bin/bash, /bin/zsh).
        /// Defaults to the configured default_program.
        #[arg(long = "shell")]
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
    /// Destroy a session.
    /// If --session is omitted and stdin is a terminal, shows an interactive picker.
    Destroy {
        #[arg(long, short)]
        session: Option<String>,
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
        /// Output idle timeout (ms). Command is considered done when no new
        /// output arrives for this duration. Default: 150ms at shell, 500ms
        /// in a subprocess (e.g. SSH, python, gdb).
        #[arg(long = "idle-timeout")]
        idle_timeout: Option<u64>,
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
    /// Send mouse event to a session
    Mouse {
        #[arg(long, short)]
        session: String,
        /// Action: click, scroll, press, release, move, drag
        action: String,
        /// Column (1-based)
        #[arg(long)]
        x: u16,
        /// Row (1-based)
        #[arg(long)]
        y: u16,
        /// Mouse button: left, middle, right (default: left)
        #[arg(long, default_value = "left")]
        button: String,
        /// Scroll direction: up, down (required for scroll action)
        #[arg(long)]
        direction: Option<String>,
        /// Repeat count for click/scroll (default: 1)
        #[arg(long)]
        count: Option<u16>,
        /// Drag target column (required for drag action)
        #[arg(long)]
        to_x: Option<u16>,
        /// Drag target row (required for drag action)
        #[arg(long)]
        to_y: Option<u16>,
        /// Drag interpolation steps (default: 5)
        #[arg(long)]
        steps: Option<u16>,
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
    /// Run as background daemon (listens on Unix socket)
    Daemon,
}

#[tokio::main]
async fn main() {
    // Daemon subcommand is detected before the tokio runtime runs its own
    // signal machinery, so we install our signal handlers first when needed.
    // We peek at argv directly to avoid running Clap before handlers are set.
    if std::env::args().nth(1).as_deref() == Some("daemon") {
        run_daemon().await;
        return;
    }

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
                    match pick_session(&socket_path, false, "attach").await {
                        Ok(Some(id)) => id,
                        Ok(None) => return, // user cancelled
                        Err(e) => {
                            eprintln!("Error: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
            };

            // Get client terminal size for sync
            let (client_rows, client_cols) = terminal_size().unwrap_or((24, 80));

            let req = Request::Attach {
                session_id: session_id.clone(),
                writable: if writable { Some(true) } else { None },
            };
            match run_attach(&socket_path, req, client_rows, client_cols).await {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Destroy { session } => {
            let config = Config::load();
            let socket_path = config.socket_path();

            // Resolve session ID: if omitted, show interactive picker.
            // include_exited=true so users can destroy already-exited sessions
            // that still occupy a slot in the daemon's session map.
            let session_id = match session {
                Some(id) => id,
                None => {
                    match pick_session(&socket_path, true, "destroy").await {
                        Ok(Some(id)) => id,
                        Ok(None) => return, // user cancelled or no sessions
                        Err(e) => {
                            eprintln!("Error: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
            };

            let req = Request::Destroy { session_id };
            let resp = match send_request(&socket_path, &req).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            };
            println!("{}", serde_json::to_string(&resp).unwrap());
            if !resp.ok {
                std::process::exit(1);
            }
        }
        Commands::Daemon => {
            // Reached only if the user explicitly runs `agent-shell daemon`
            // after the tokio runtime is already up. Redirect to the same path.
            run_daemon().await;
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

            let args: Option<Vec<String>> = shell.map(|s| vec![s]);

            let cwd = cwd.or_else(|| std::env::current_dir().ok()
                .map(|p| p.to_string_lossy().into_owned()));

            Request::Create {
                name,
                program: None,
                args,
                cwd,
                env,
                prompt,
                rows,
                cols,
                buffer_size,
                record: if record { Some(true) } else { None },
            }
        }
        Commands::Destroy { session } => Request::Destroy {
            // session is always Some here: the None case is handled in main()
            // before command_to_request is called.
            session_id: session.unwrap_or_default(),
        },
        Commands::Send { session, text, ctrl, nowait, timeout, idle_timeout, client_id } => Request::Send {
            session_id: session,
            text: text.unwrap_or_default(),
            ctrl,
            nowait: if nowait { Some(true) } else { None },
            timeout_ms: timeout,
            idle_timeout_ms: idle_timeout,
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
        Commands::Mouse { session, action, x, y, button, direction, count, to_x, to_y, steps } => Request::Mouse {
            session_id: session,
            action,
            x,
            y,
            button: Some(button),
            direction,
            count,
            to_x,
            to_y,
            steps,
        },
        Commands::Stop => Request::Stop,
        Commands::KillDaemon => unreachable!("KillDaemon is handled locally, never sent via socket"),
        Commands::Daemon => unreachable!("Daemon is handled before command_to_request is called"),
        Commands::Replay { .. } => unreachable!(),
    }
}

// ─── Normal (non-streaming) command ──────────────────────────────────

/// Send a single `Request` to the daemon and return its `Response`.
/// Does not auto-start the daemon; callers that want auto-start should
/// use `connect_and_send` instead.
async fn send_request(socket_path: &PathBuf, req: &Request) -> Result<Response, String> {
    let mut stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .map_err(|e| format!("connect: {}", e))?;

    let data = serde_json::to_vec(req).map_err(|e| format!("serialize: {}", e))?;
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes()).await.map_err(|e| format!("write: {}", e))?;
    stream.write_all(&data).await.map_err(|e| format!("write: {}", e))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(|e| format!("read: {}", e))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        return Err("response too large".into());
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.map_err(|e| format!("read: {}", e))?;

    serde_json::from_slice(&buf).map_err(|e| format!("deserialize: {}", e))
}

async fn connect_and_send(socket_path: &PathBuf, cmd: &Commands) -> Result<Response, String> {
    let req = command_to_request(cmd.clone());
    send_request(socket_path, &req).await
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

    let frame = agent_shell_core::attach::request_frame(&req)?;
    stream.write_all(&frame).await.map_err(|e| format!("write: {}", e))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(|e| format!("read: {}", e))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        return Err("response too large".into());
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.map_err(|e| format!("read: {}", e))?;

    let resp: Response = serde_json::from_slice(&buf).map_err(|e| format!("deserialize: {}", e))?;
    if !resp.ok {
        return Err(resp.error.unwrap_or_else(|| "list failed".into()));
    }
    Ok(resp.sessions.unwrap_or_default())
}

/// Show an interactive session picker and return the selected session ID.
///
/// `include_exited`: when true, exited sessions are also shown (used by destroy).
/// `command_hint`:   name of the flag to use when stdin is not a terminal
///                   (shown in the fallback error message).
///
/// Returns Ok(None) if the user cancels (Esc/Ctrl-C) or there are no sessions.
async fn pick_session(
    socket_path: &PathBuf,
    include_exited: bool,
    command_hint: &str,
) -> Result<Option<String>, String> {
    let sessions = fetch_sessions(socket_path).await?;

    let candidates: Vec<_> = if include_exited {
        sessions
    } else {
        sessions.into_iter().filter(|s| !s.exited).collect()
    };

    if candidates.is_empty() {
        if include_exited {
            eprintln!("No sessions found.");
        } else {
            eprintln!("No active sessions. Create one with: agent-shell create");
        }
        return Ok(None);
    }

    // Interactive picker requires a terminal on stdin.
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "{} session(s) available but stdin is not a terminal.",
            candidates.len()
        );
        eprintln!("Specify a session with: agent-shell {} --session <ID>", command_hint);
        eprintln!("\nSessions:");
        for s in &candidates {
            eprintln!("  {}", session_label(s));
        }
        return Ok(None);
    }

    let items: Vec<String> = candidates.iter().map(|s| session_label(s)).collect();

    let selection = dialoguer::Select::new()
        .with_prompt("Select session")
        .items(&items)
        .default(0)
        .interact_opt()
        .map_err(|e| format!("picker: {}", e))?;

    match selection {
        Some(idx) => Ok(Some(candidates[idx].id.clone())),
        None => Ok(None),
    }
}

/// Format a session label for display.
fn session_label(s: &agent_shell_core::protocol::SessionInfo) -> String {
    let name = s.name.as_deref().unwrap_or("<unnamed>");
    // Show the short program name (basename) instead of the full path.
    let prog = s.program.as_deref()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .unwrap_or("?");
    let cwd = s.cwd.as_deref().unwrap_or("?");
    format!("{} ({}, {}, {})", s.id, name, prog, cwd)
}

// ─── Attach: bidirectional raw streaming ─────────────────────────────

/// Resize session via a separate connection (before attach enters binary mode).
async fn resize_via_separate_connection(socket_path: &PathBuf, session_id: &str, rows: u16, cols: u16) -> Result<(), String> {
    let mut stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .map_err(|e| format!("resize connect: {}", e))?;
    let resize_req = Request::Resize {
        session_id: session_id.to_string(),
        rows,
        cols,
    };
    let data = serde_json::to_vec(&resize_req).map_err(|e| format!("serialize resize: {}", e))?;
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes()).await.map_err(|e| format!("write resize: {}", e))?;
    stream.write_all(&data).await.map_err(|e| format!("write resize: {}", e))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(|e| format!("read resize resp: {}", e))?;
    let resp_len = u32::from_be_bytes(len_buf) as usize;
    if resp_len > 16 * 1024 * 1024 {
        return Err("resize response too large".into());
    }
    let mut buf = vec![0u8; resp_len];
    stream.read_exact(&mut buf).await.map_err(|e| format!("read resize resp: {}", e))?;
    Ok(())
}

async fn run_attach(socket_path: &PathBuf, req: Request, client_rows: u16, client_cols: u16) -> Result<(), String> {
    let readonly = !matches!(&req, Request::Attach { writable: Some(true), .. });
    let session_id_for_resize = match &req {
        Request::Attach { session_id, .. } => session_id.clone(),
        _ => String::new(),
    };

    // ── Phase 0: resize session BEFORE attach (via separate connection) ──
    // Attach enters binary-stream mode, so resize must happen on its own
    // connection first. This ensures vim/TUI apps see the correct terminal
    // size from the very first byte of output.
    if !session_id_for_resize.is_empty() {
        let _ = resize_via_separate_connection(
            socket_path, &session_id_for_resize, client_rows, client_cols,
        ).await;
    }

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

    // ── Phase 1: send attach request & read JSON handshake ────────────────

    let data = serde_json::to_vec(&req).map_err(|e| format!("serialize: {}", e))?;
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes()).await.map_err(|e| format!("write: {}", e))?;
    stream.write_all(&data).await.map_err(|e| format!("write: {}", e))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(|e| format!("read handshake: {}", e))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > agent_shell_core::attach::MAX_HANDSHAKE_RESPONSE_BYTES {
        return Err("handshake response too large".into());
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.map_err(|e| format!("read handshake: {}", e))?;
    let resp = agent_shell_core::attach::decode_response(&buf)?;

    if !resp.ok {
        // Print error as JSON and exit
        println!("{}", serde_json::to_string(&resp).unwrap());
        return Err(resp.error.unwrap_or_else(|| "attach failed".into()));
    }

    // Phase 2: raw binary bidirectional streaming.
    // Enter raw mode BEFORE writing the initial snapshot.
    let _raw_guard = enter_raw_mode();

    if resp.output.is_some() {
        let output = agent_shell_core::attach::initial_output(&resp);
        let _ = std::io::stdout().write_all(&output);
        let _ = std::io::stdout().flush();
    }

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
                            // Forward all input (including Ctrl-C) to the PTY.
                            // The shell/readline inside the session handles Ctrl-C
                            // (clear current line, print ^C). We must not swallow it.
                            if stream_tx.write_all(data).await.is_err() {
                                running = false;
                            }
                        }
                        Err(_) => running = false,
                    }
                }
            }
        }
    }

    // Reset terminal state before restoring cooked mode.
    // Without this the outer shell's prompt is visually "stuck" because:
    //  - The cursor may be mid-screen after the PTY session's last output.
    //  - SGR attributes (colors, bold) may still be active.
    //  - The outer shell does not know the screen changed, so it won't
    //    redraw until the user presses a key.
    // Writing \r\n ensures the next shell prompt appears on a fresh line.
    {
        use std::io::Write;
        let _ = std::io::stdout().write_all(b"\x1b[0m\r\n");
        let _ = std::io::stdout().flush();
    }

    // _raw_guard dropped here → terminal restored
    Ok(())
}

// ─── Daemon auto-start ───────────────────────────────────────────────

async fn auto_start_daemon(socket_path: &PathBuf) -> Result<(), String> {
    // Try connecting first — another daemon may already be running
    if socket_path.exists() {
        if tokio::net::UnixStream::connect(socket_path).await.is_ok() {
            return Ok(()); // daemon is running, just use it
        }
        // Socket file exists but no daemon — stale artifact, safe to remove
        let _ = std::fs::remove_file(socket_path);
    }
    let self_bin = find_self_binary()?;
    std::process::Command::new(&self_bin)
        .arg("daemon")
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

fn find_self_binary() -> Result<String, String> {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("cannot determine own executable path: {}", e))
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

// ─── Daemon entry point ─────────────────────────────────────────────

async fn run_daemon() {
    let config = Config::load();
    let base_dir = Config::base_dir();
    let _ = std::fs::create_dir_all(&base_dir);

    install_signal_handlers();

    let pid_path = base_dir.join("daemon.pid");
    let _ = std::fs::write(&pid_path, std::process::id().to_string());

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let shutdown_trigger = shutdown_tx.clone();
    tokio::spawn(async move {
        loop {
            if SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
                let _ = shutdown_trigger.send(true);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });

    let socket_path = config.socket_path();
    let socket_path_for_cleanup = socket_path.clone();
    let pid_path_for_cleanup = pid_path.clone();

    if let Err(e) = server::run(socket_path, config, shutdown_rx).await {
        eprintln!("daemon error: {}", e);
    }

    let _ = std::fs::remove_file(&socket_path_for_cleanup);
    let _ = std::fs::remove_file(&pid_path_for_cleanup);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_create(shell: Option<&str>) -> Request {
        command_to_request(Commands::Create {
            name: None,
            shell: shell.map(|s| s.to_string()),
            cwd: None,
            envs: None,
            prompt: None,
            rows: None,
            cols: None,
            buffer_size: None,
            record: false,
        })
    }

    #[test]
    fn no_shell_passes_none_args() {
        // agent-shell create  ->  daemon uses default_program
        match make_create(None) {
            Request::Create { args, program, .. } => {
                assert_eq!(args, None);
                assert_eq!(program, None);
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn shell_becomes_single_argv() {
        // agent-shell create --shell /bin/zsh  ->  ["/bin/zsh"]
        match make_create(Some("/bin/zsh")) {
            Request::Create { args, .. } => {
                assert_eq!(args, Some(vec!["/bin/zsh".to_string()]));
            }
            _ => panic!("expected Create"),
        }
    }
}
