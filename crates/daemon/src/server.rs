use agent_shell_core::config::Config;
use agent_shell_core::protocol::{Request, Response, SessionInfo};
use agent_shell_core::session::Session;
use std::collections::HashMap;
use nix::libc;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::Mutex;

/// Per-client read cursor state.
#[derive(Debug)]
struct ClientState {
    read_cursor: u64,
    last_active: std::time::Instant,
}

struct AppState {
    config: Config,
    sessions: HashMap<String, Session>,
    clients: HashMap<String, ClientState>,
}

pub async fn run(socket_path: std::path::PathBuf, config: Config) -> Result<(), String> {
    // Clean up stale socket
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    // Ensure parent dir exists
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| format!("bind socket {:?}: {}", socket_path, e))?;

    // Set socket permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o700));
    }

    eprintln!("agent-shell daemon listening on {:?}", socket_path);

    let state = Arc::new(Mutex::new(AppState {
        config,
        sessions: HashMap::new(),
        clients: HashMap::new(),
    }));

    // Cleanup clients that have been inactive for > 10 minutes
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let mut s = cleanup_state.lock().await;
            let now = std::time::Instant::now();
            s.clients.retain(|_, c| now.duration_since(c.last_active).as_secs() < 600);
        }
    });

    // Background reaper: periodically check all sessions for exited children
    // and reap zombies. This ensures idle sessions don't leave <defunct>
    // processes when their child exits outside of a send/wait cycle.
    let reaper_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
        loop {
            interval.tick().await;
            let mut s = reaper_state.lock().await;
            for session in s.sessions.values_mut() {
                if session.exited.is_none() {
                    session.feed();
                    session.check_exited();
                }
            }
        }
    });

    // Accept connections
    loop {
        let (stream, _) = listener.accept().await.map_err(|e| format!("accept: {}", e))?;
        let state = state.clone();
        tokio::spawn(async move {
            handle_connection(stream, state).await;
        });
    }
}

async fn handle_connection(mut stream: tokio::net::UnixStream, state: Arc<Mutex<AppState>>) {
    // Read length-prefixed JSON request
    let mut len_buf = [0u8; 4];
    if stream.read_exact(&mut len_buf).await.is_err() {
        return;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        return;
    }
    let mut buf = vec![0u8; len];
    if stream.read_exact(&mut buf).await.is_err() {
        return;
    }
    let req: Request = match serde_json::from_slice(&buf) {
        Ok(r) => r,
        Err(e) => {
            let resp = Response::err(format!("invalid request: {}", e));
            send_response(&mut stream, &resp).await;
            return;
        }
    };

    // Handle attach specially (streaming)
    if let Request::Attach {
        session_id,
        readonly,
    } = req
    {
        handle_attach(stream, state, session_id, readonly.unwrap_or(false)).await;
        return;
    }

    let resp = handle_request(req, state).await;
    send_response(&mut stream, &resp).await;
}

async fn send_response(stream: &mut tokio::net::UnixStream, resp: &Response) {
    if let Ok(data) = serde_json::to_vec(resp) {
        let len = data.len() as u32;
        let _ = stream.write_all(&len.to_be_bytes()).await;
        let _ = stream.write_all(&data).await;
    }
}

async fn handle_request(req: Request, state: Arc<Mutex<AppState>>) -> Response {
    match req {
        Request::Create {
            name,
            shell,
            cwd,
            env,
            prompt,
            rows,
            cols,
            buffer_size,
            record,
        } => {
            let cwd = cwd.map(std::path::PathBuf::from);
            handle_create(state, name, shell, cwd, env, prompt, rows, cols, buffer_size, record).await
        }

        Request::Destroy { session_id } => handle_destroy(state, session_id).await,

        Request::Send {
            session_id,
            text,
            ctrl,
            nowait,
            timeout_ms,
            client_id,
        } => {
            handle_send(state, session_id, text, ctrl, nowait.unwrap_or(false), timeout_ms, client_id).await
        }

        Request::Read {
            session_id,
            client_id,
            screen,
        } => handle_read(state, session_id, client_id, screen.unwrap_or(false)).await,

        Request::Wait {
            session_id,
            pattern,
            fixed,
            timeout_ms,
            client_id,
        } => handle_wait(state, session_id, pattern, fixed.unwrap_or(false), timeout_ms, client_id).await,

        Request::SetPrompt { session_id, prompt } => {
            handle_set_prompt(state, session_id, prompt).await
        }

        Request::List => handle_list(state).await,

        Request::Resize {
            session_id,
            rows,
            cols,
        } => handle_resize(state, session_id, rows, cols).await,

        Request::Stop => handle_stop(state).await,

        Request::Attach { .. } => unreachable!(),
    }
}

async fn handle_create(
    state: Arc<Mutex<AppState>>,
    name: Option<String>,
    shell: Option<String>,
    cwd: Option<std::path::PathBuf>,
    env: Option<HashMap<String, String>>,
    prompt: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
    buffer_size: Option<usize>,
    record: Option<bool>,
) -> Response {
    let config;
    {
        let s = state.lock().await;
        config = s.config.clone();
    }

    match Session::new(&config, name, shell, cwd, env, prompt, rows, cols, buffer_size, record) {
        Ok(mut session) => {
            let id = session.id.clone();
            let recording_path = session.recording.as_ref().map(|_| {
                config.recording_dir().join(format!("{}.jsonl", id)).to_string_lossy().to_string()
            });

            // Set PTY reader to non-blocking
            set_nonblocking(session.master_fd());

            // Wait briefly for shell to start
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            session.feed();
            session.check_fg_pgid();

            let mut s = state.lock().await;
            s.sessions.insert(id.clone(), session);

            Response {
                ok: true,
                session_id: Some(id),
                prompt_detected: Some(None),
                recording: recording_path,
                ..Response::ok()
            }
        }
        Err(e) => Response::err(e),
    }
}

/// Set a file descriptor to non-blocking mode.
fn set_nonblocking(fd: Option<std::os::unix::io::RawFd>) {
    if let Some(fd) = fd {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
    }
}

async fn handle_destroy(state: Arc<Mutex<AppState>>, session_id: String) -> Response {
    let mut s = state.lock().await;
    match s.sessions.get_mut(&session_id) {
        Some(session) => {
            session.kill();
            session.close_recording();
            std::thread::sleep(std::time::Duration::from_millis(50));
            session.force_kill(); // SIGKILL + reap
            s.sessions.remove(&session_id); // Drop triggers Session::drop -> try_wait
            Response {
                ok: true,
                session_id: Some(session_id),
                ..Response::ok()
            }
        }
        None => Response::err("session not found"),
    }
}

async fn handle_send(
    state: Arc<Mutex<AppState>>,
    session_id: String,
    text: String,
    ctrl: Option<String>,
    nowait: bool,
    timeout_ms: Option<u64>,
    client_id: Option<String>,
) -> Response {
    let timeout_ms = timeout_ms.unwrap_or(agent_shell_core::session::DEFAULT_TIMEOUT_MS);
    let start = std::time::Instant::now();

    // Step 1: Send text/ctrl and record state
    let seq;
    let write_cursor_before;
    {
        let mut s = state.lock().await;
        let session = match s.sessions.get_mut(&session_id) {
            Some(s) => s,
            None => return Response::err("session not found"),
        };

        if let Some(exit_code) = session.exited {
            return Response {
                ok: false,
                error: Some("session exited".into()),
                exit_code: Some(exit_code),
                ..Response::ok()
            };
        }

        seq = session.next_seq();

        if let Some(ref ctrl) = ctrl {
            if let Err(e) = session.send_ctrl(ctrl) {
                return Response::err(e);
            }
        } else if !text.is_empty() {
            if let Err(e) = session.send_text(&text) {
                return Response::err(e);
            }
        }

        // Drain any immediate output
        session.feed();
        write_cursor_before = session.ringbuf.write_cursor();
    }

    // Step 2: If nowait or ctrl, return immediately
    if nowait || ctrl.is_some() {
        let mut s = state.lock().await;
        let session = s.sessions.get_mut(&session_id).unwrap();
        session.feed();

        let (output, gap, lost_bytes) = session.ringbuf.read(write_cursor_before);
        // Also advance the client cursor if client_id provided
        if let Some(ref cid) = client_id {
            let wc = session.ringbuf.write_cursor();
            s.clients.insert(
                cid.clone(),
                ClientState {
                    read_cursor: wc,
                    last_active: std::time::Instant::now(),
                },
            );
        }

        let mut resp = Response {
            ok: true,
            seq: Some(seq),
            output: Some(String::from_utf8_lossy(&output).to_string()),
            elapsed_ms: Some(start.elapsed().as_millis() as u64),
            ..Response::ok()
        };
        if gap {
            resp.gap = Some(true);
            resp.lost_bytes = Some(lost_bytes);
        }
        return resp;
    }

    // Step 3: Wait for readiness signal
    // Strategy: Two-phase approach.
    // Phase 1: Wait for fg_pgid to leave shell (command started) with a short timeout.
    // Phase 2: Wait for fg_pgid to return to shell, or prompt match, or exit.
    // For very fast commands that complete before Phase 1 timeout, we use output
    // stabilization (no new output for 150ms while fg_at_shell).
    let deadline = start + std::time::Duration::from_millis(timeout_ms);
    let mut stable_since: Option<std::time::Instant> = None;
    let mut last_wc = write_cursor_before;

    loop {
        if std::time::Instant::now() >= deadline {
            let mut s = state.lock().await;
            let session = s.sessions.get_mut(&session_id).unwrap();
            session.feed();

            let (output, gap, lost_bytes) = session.ringbuf.read(write_cursor_before);

            let mut resp = Response {
                ok: false,
                seq: Some(seq),
                error: Some("timeout".into()),
                output: Some(String::from_utf8_lossy(&output).to_string()),
                elapsed_ms: Some(start.elapsed().as_millis() as u64),
                ..Response::ok()
            };
            if gap {
                resp.gap = Some(true);
                resp.lost_bytes = Some(lost_bytes);
            }
            return resp;
        }

        // Check readiness
        let (fg_at_shell, prompt_matched, exit_code, wc, overflowed, lost) = {
            let mut s = state.lock().await;
            let session = match s.sessions.get_mut(&session_id) {
                Some(s) => s,
                None => return Response::err("session not found"),
            };

            session.feed();
            let wc = session.ringbuf.write_cursor();

            session.check_fg_pgid();
            let fg_at_shell = session.current_fg_pgid == session.shell_pgid;

            let exit_code = session.check_exited();

            // Check prompt regex match in new output
            let prompt_matched = if let Some(ref regex) = session.prompt_regex {
                if wc > write_cursor_before {
                    let (new_data, _, _) = session.ringbuf.read(write_cursor_before);
                    let text = String::from_utf8_lossy(&new_data);
                    regex.is_match(&text)
                } else {
                    false
                }
            } else {
                false
            };

            let (overflowed, lost) = session.ringbuf.take_overflow();

            (fg_at_shell, prompt_matched, exit_code, wc, overflowed, lost)
        };

        if overflowed {
            return Response {
                ok: false,
                seq: Some(seq),
                error: Some("buffer_overflow".into()),
                lost_bytes: Some(lost),
                elapsed_ms: Some(start.elapsed().as_millis() as u64),
                ..Response::ok()
            };
        }

        // Track output stabilization
        if wc != last_wc {
            stable_since = None;
            last_wc = wc;
        } else if fg_at_shell {
            stable_since.get_or_insert(std::time::Instant::now());
        }

        // Ready conditions:
        // 1. Prompt matched → interactive program ready
        // 2. Process exited → done
        // 3. fg at shell AND output has stabilized (no new output for 150ms) → command done
        // 4. fg at shell AND new output AND we've been waiting at least 200ms → fast command done
        let has_new_output = wc > write_cursor_before;
        let elapsed = start.elapsed();
        let output_stable = stable_since
            .map(|t| t.elapsed().as_millis() >= 150)
            .unwrap_or(false);

        if prompt_matched || exit_code.is_some() || (fg_at_shell && has_new_output && output_stable)
            || (fg_at_shell && has_new_output && elapsed.as_millis() >= 200)
        {
            let mut s = state.lock().await;
            let session = s.sessions.get_mut(&session_id).unwrap();
            let (output, gap, lost_bytes) = session.ringbuf.read(write_cursor_before);
            if let Some(ref cid) = client_id {
                let wc = session.ringbuf.write_cursor();
                s.clients.insert(
                    cid.clone(),
                    ClientState {
                        read_cursor: wc,
                        last_active: std::time::Instant::now(),
                    },
                );
            }

            let mut resp = Response {
                ok: true,
                seq: Some(seq),
                output: Some(String::from_utf8_lossy(&output).to_string()),
                elapsed_ms: Some(start.elapsed().as_millis() as u64),
                ..Response::ok()
            };

            if gap {
                resp.gap = Some(true);
                resp.lost_bytes = Some(lost_bytes);
            }

            if let Some(code) = exit_code {
                resp.exited = Some(true);
                resp.exit_code = Some(code);
            }

            return resp;
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

fn read_from_cursor(
    state: &mut AppState,
    session_id: &str,
    client_id: &Option<String>,
) -> (Vec<u8>, bool, u64) {
    let client_id_val = client_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let cursor = state.clients.get(&client_id_val).map(|c| c.read_cursor).unwrap_or(0);

    let session = match state.sessions.get(session_id) {
        Some(s) => s,
        None => return (Vec::new(), false, 0),
    };

    let (data, gap, lost) = session.ringbuf.read(cursor);
    let new_cursor = session.ringbuf.write_cursor();

    state.clients.insert(
        client_id_val,
        ClientState {
            read_cursor: new_cursor,
            last_active: std::time::Instant::now(),
        },
    );

    (data, gap, lost)
}

async fn handle_read(
    state: Arc<Mutex<AppState>>,
    session_id: String,
    client_id: Option<String>,
    screen: bool,
) -> Response {
    let mut s = state.lock().await;

    if screen {
        let session = match s.sessions.get_mut(&session_id) {
            Some(s) => s,
            None => return Response::err("session not found"),
        };
        let screen_data = session.vte_grid.screen();
        let cursor = session.vte_grid.cursor();
        return Response {
            ok: true,
            screen: Some(screen_data),
            cursor: Some(cursor),
            ..Response::ok()
        };
    }

    let session = match s.sessions.get_mut(&session_id) {
        Some(s) => s,
        None => return Response::err("session not found"),
    };

    if let Some(exit_code) = session.exited {
        let (output, gap, lost_bytes) = read_from_cursor(&mut s, &session_id, &client_id);
        let mut resp = Response {
            ok: true,
            output: Some(String::from_utf8_lossy(&output).to_string()),
            exited: Some(true),
            exit_code: Some(exit_code),
            ..Response::ok()
        };
        if gap {
            resp.gap = Some(true);
            resp.lost_bytes = Some(lost_bytes);
        }
        if output.is_empty() {
            resp.ok = false;
            resp.error = Some("session exited".into());
        }
        return resp;
    }

    session.feed();
    let (output, gap, lost_bytes) = read_from_cursor(&mut s, &session_id, &client_id);

    let mut resp = Response {
        ok: true,
        output: Some(String::from_utf8_lossy(&output).to_string()),
        ..Response::ok()
    };
    if gap {
        resp.gap = Some(true);
        resp.lost_bytes = Some(lost_bytes);
    }
    resp
}

async fn handle_wait(
    state: Arc<Mutex<AppState>>,
    session_id: String,
    pattern: String,
    fixed: bool,
    timeout_ms: Option<u64>,
    client_id: Option<String>,
) -> Response {
    let timeout_ms = timeout_ms.unwrap_or(agent_shell_core::session::DEFAULT_TIMEOUT_MS);
    let start = std::time::Instant::now();
    let deadline = start + std::time::Duration::from_millis(timeout_ms);

    let regex = if fixed {
        regex::Regex::new(&regex::escape(&pattern)).ok()
    } else {
        match regex::Regex::new(&pattern) {
            Ok(r) => Some(r),
            Err(e) => return Response::err(format!("invalid regex: {}", e)),
        }
    };
    let regex = match regex {
        Some(r) => r,
        None => return Response::err("invalid regex"),
    };

    let client_id_val = client_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    loop {
        if std::time::Instant::now() >= deadline {
            return Response {
                ok: false,
                error: Some("timeout".into()),
                elapsed_ms: Some(start.elapsed().as_millis() as u64),
                ..Response::ok()
            };
        }

        let result = {
            let mut s = state.lock().await;

            let cursor = s.clients.get(&client_id_val).map(|c| c.read_cursor).unwrap_or(0);

            let session = match s.sessions.get_mut(&session_id) {
                Some(s) => s,
                None => return Response::err("session not found"),
            };

            session.feed();

            let wc = session.ringbuf.write_cursor();

            let exit_code = session.check_exited();

            if wc > cursor {
                let (data, _, _) = session.ringbuf.read(cursor);
                let text = String::from_utf8_lossy(&data);
                if regex.is_match(&text) {
                    let new_cursor = session.ringbuf.write_cursor();
                    s.clients.insert(
                        client_id_val.clone(),
                        ClientState {
                            read_cursor: new_cursor,
                            last_active: std::time::Instant::now(),
                        },
                    );

                    let mut resp = Response {
                        ok: true,
                        output: Some(text.to_string()),
                        elapsed_ms: Some(start.elapsed().as_millis() as u64),
                        ..Response::ok()
                    };
                    if let Some(code) = exit_code {
                        resp.exited = Some(true);
                        resp.exit_code = Some(code);
                    }
                    return resp;
                }
            }

            if let Some(code) = exit_code {
                let (data, _, _) = session.ringbuf.read(cursor);
                let text = String::from_utf8_lossy(&data);
                let new_cursor = session.ringbuf.write_cursor();
                s.clients.insert(
                    client_id_val,
                    ClientState {
                        read_cursor: new_cursor,
                        last_active: std::time::Instant::now(),
                    },
                );
                return Response {
                    ok: false,
                    error: Some("session exited".into()),
                    exit_code: Some(code),
                    output: Some(text.to_string()),
                    elapsed_ms: Some(start.elapsed().as_millis() as u64),
                    ..Response::ok()
                };
            }

            None::<Response>
        };

        if let Some(resp) = result {
            return resp;
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn handle_set_prompt(
    state: Arc<Mutex<AppState>>,
    session_id: String,
    prompt: Option<String>,
) -> Response {
    let mut s = state.lock().await;
    let session = match s.sessions.get_mut(&session_id) {
        Some(s) => s,
        None => return Response::err("session not found"),
    };

    match prompt {
        Some(p) if !p.is_empty() => match regex::Regex::new(&p) {
            Ok(r) => {
                session.prompt_regex = Some(r);
                Response {
                    ok: true,
                    session_id: Some(session_id),
                    ..Response::ok()
                }
            }
            Err(e) => Response::err(format!("invalid regex: {}", e)),
        },
        _ => {
            session.prompt_regex = None;
            Response {
                ok: true,
                session_id: Some(session_id),
                ..Response::ok()
            }
        }
    }
}

async fn handle_list(state: Arc<Mutex<AppState>>) -> Response {
    let s = state.lock().await;
    let sessions: Vec<SessionInfo> = s
        .sessions
        .values()
        .map(|session| SessionInfo {
            id: session.id.clone(),
            name: session.name.clone(),
            prompt: session.prompt_regex.as_ref().map(|r| r.to_string()),
            exited: session.exited.is_some(),
            exit_code: session.exited,
            pid: session.child_pid,
            created_at: session.created_at.elapsed().as_secs(),
            buffer_size: session.buffer_size,
            recording: session.recording.is_some(),
        })
        .collect();

    Response {
        ok: true,
        sessions: Some(sessions),
        ..Response::ok()
    }
}

async fn handle_resize(
    state: Arc<Mutex<AppState>>,
    session_id: String,
    rows: u16,
    cols: u16,
) -> Response {
    let mut s = state.lock().await;
    let session = match s.sessions.get_mut(&session_id) {
        Some(s) => s,
        None => return Response::err("session not found"),
    };

    match session.resize(rows, cols) {
        Ok(()) => Response {
            ok: true,
            session_id: Some(session_id),
            ..Response::ok()
        },
        Err(e) => Response::err(e),
    }
}

async fn handle_stop(state: Arc<Mutex<AppState>>) -> Response {
    let mut s = state.lock().await;
    for session in s.sessions.values_mut() {
        session.kill();
        session.close_recording();
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    for session in s.sessions.values_mut() {
        session.force_kill(); // SIGKILL + reap
    }
    s.sessions.clear(); // Drop all sessions -> Session::drop -> try_wait

    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        std::process::exit(0);
    });

    Response::ok()
}

async fn handle_attach(
    mut stream: tokio::net::UnixStream,
    state: Arc<Mutex<AppState>>,
    session_id: String,
    readonly: bool,
) {
    // ── Phase 1: validate & send initial JSON handshake ──────────────────
    let (redraw_bytes, start_cursor) = {
        let mut s = state.lock().await;
        let session = match s.sessions.get_mut(&session_id) {
            Some(s) => s,
            None => {
                send_response(&mut stream, &Response::err("session not found")).await;
                return;
            }
        };
        if session.exited.is_some() {
            send_response(&mut stream, &Response::err("session exited")).await;
            return;
        }
        let wc = session.ringbuf.write_cursor();
        let redraw = session.vte_grid.full_redraw_bytes();
        (redraw, wc)
    };

    // JSON response carries the VT100 full-screen redraw.
    // serde_json will correctly round-trip the control chars.
    let init_resp = Response {
        ok: true,
        output: Some(String::from_utf8_lossy(&redraw_bytes).to_string()),
        ..Response::ok()
    };
    send_response(&mut stream, &init_resp).await;

    // ── Phase 2: raw binary bidirectional streaming ─────────────────────
    // After the length-prefixed JSON, we switch to raw binary:
    //   client → daemon: raw keystroke bytes
    //   daemon → client: raw PTY output bytes
    let (mut stream_rx, mut stream_tx) = stream.into_split();
    let mut pty_cursor = start_cursor;
    let mut running = true;

    while running {
        let mut stdin_buf = [0u8; 4096];

        if readonly {
            // ── readonly: only poll PTY ──
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        } else {
            // ── rw: select between client-stdin and PTY-poll ──
            tokio::select! {
                // client keystroke → PTY
                result = stream_rx.read(&mut stdin_buf) => {
                    match result {
                        Ok(0) => { running = false; continue; }
                        Ok(n) => {
                            let data = &stdin_buf[..n];
                            let mut s = state.lock().await;
                            if let Some(session) = s.sessions.get_mut(&session_id) {
                                let _ = session.pty_writer.write_all(data);
                                let _ = session.pty_writer.flush();
                                if let Some(ref mut rec) = session.recording {
                                    rec.record_in(data);
                                }
                            } else {
                                running = false;
                            }
                        }
                        Err(_) => { running = false; continue; }
                    }
                    // Also drain any PTY output that the keystroke may have produced
                }
                // PTY poll timer
                _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
            }
        }

        // ── poll PTY output → client ──
        let (data, exited, gone) = {
            let mut s = state.lock().await;
            match s.sessions.get_mut(&session_id) {
                Some(session) => {
                    let exited = session.exited.is_some();
                    session.feed();
                    let wc = session.ringbuf.write_cursor();
                    let data = if wc > pty_cursor {
                        let (d, _, _) = session.ringbuf.read(pty_cursor);
                        pty_cursor = wc;
                        d
                    } else {
                        Vec::new()
                    };
                    (data, exited, false)
                }
                None => (Vec::new(), false, true),
            }
        }; // lock released

        if !data.is_empty() {
            if stream_tx.write_all(&data).await.is_err() {
                break;
            }
        }
        if exited || gone {
            running = false;
        }
    }
}
