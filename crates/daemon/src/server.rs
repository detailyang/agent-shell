use agent_shell_core::config::Config;
use agent_shell_core::protocol::{Request, Response, SessionInfo};
use agent_shell_core::session::Session;
use std::collections::HashMap;
use nix::libc;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::{Mutex, Notify, watch};

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
    /// Notified whenever any session's PTY produces new output.
    /// attach loops await this instead of a fixed 50 ms poll so that
    /// PTY→terminal latency is near-zero (important for ESC[6n CPR round-trips).
    pty_output_notify: Arc<Notify>,
}

pub async fn run(
    socket_path: std::path::PathBuf,
    config: Config,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), String> {
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

    let pty_notify = Arc::new(Notify::new());
    let state = Arc::new(Mutex::new(AppState {
        config,
        sessions: HashMap::new(),
        clients: HashMap::new(),
        pty_output_notify: pty_notify,
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

    // Background reaper: periodically drain all PTY masters and reap zombies.
    // After each feed() round we notify waiting attach loops so they can
    // forward new PTY output to clients without waiting for the 50 ms poll.
    let reaper_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
        loop {
            interval.tick().await;
            let notify = {
                let mut s = reaper_state.lock().await;
                let mut any_new = false;
                for session in s.sessions.values_mut() {
                    if session.exited.is_none() {
                        let n = session.feed();
                        if n > 0 { any_new = true; }
                        session.check_exited();
                    }
                }
                if any_new { Some(s.pty_output_notify.clone()) } else { None }
            };
            if let Some(n) = notify { n.notify_waiters(); }
        }
    });

    // Accept connections (or shutdown)
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, _) = accept_result.map_err(|e| format!("accept: {}", e))?;
                let state = state.clone();
                tokio::spawn(async move {
                    handle_connection(stream, state).await;
                });
            }
            _ = shutdown_rx.changed() => {
                // Graceful shutdown requested (SIGTERM/SIGINT)
                eprintln!("agent-shell daemon: shutdown requested");
                // Kill all sessions
                {
                    let mut s = state.lock().await;
                    for session in s.sessions.values_mut() {
                        session.kill();
                        session.close_recording();
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                {
                    let mut s = state.lock().await;
                    for session in s.sessions.values_mut() {
                        session.force_kill();
                    }
                    s.sessions.clear();
                }
                return Ok(());
            }
        }
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
        writable,
    } = req
    {
        handle_attach(stream, state, session_id, !writable.unwrap_or(false)).await;
        return;
    }

    // Handle stop: send OK response, then shut down
    if let Request::Stop = req {
        send_response(&mut stream, &Response::ok()).await;
        handle_stop(state).await; // does not return
        unreachable!()
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
            program,
            args,
            cwd,
            env,
            prompt,
            rows,
            cols,
            buffer_size,
            record,
        } => {
            let cwd = cwd.map(std::path::PathBuf::from);
            handle_create(state, name, program, args, cwd, env, prompt, rows, cols, buffer_size, record).await
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

        Request::Mouse {
            session_id,
            action,
            x,
            y,
            button,
            direction,
            count,
            to_x,
            to_y,
            steps,
        } => handle_mouse(state, session_id, action, x, y, button, direction, count, to_x, to_y, steps).await,

        Request::Stop => unreachable!("Stop is handled in handle_connection"),
        Request::Attach { .. } => unreachable!(),
    }
}

async fn handle_create(
    state: Arc<Mutex<AppState>>,
    name: Option<String>,
    program: Option<String>,
    args: Option<Vec<String>>,
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

    match Session::new(&config, name, program, args, cwd, env, prompt, rows, cols, buffer_size, record) {
        Ok(session) => {
            let id = session.id.clone();
            let recording_path = session.recording.as_ref().map(|_| {
                config.recording_dir().join(format!("{}.jsonl", id)).to_string_lossy().to_string()
            });

            // Set PTY reader to non-blocking BEFORE inserting into map
            if let Err(e) = set_nonblocking(session.master_fd()) {
                eprintln!("warning: failed to set PTY non-blocking: {}", e);
                // Non-fatal: feed() will still work, just may block briefly
            }

            // Insert session into map first so the reaper / shutdown tasks
            // can see it immediately.
            {
                let mut s = state.lock().await;
                s.sessions.insert(id.clone(), session);
            } // lock released before sleep

            // Wait briefly for the shell to initialise WITHOUT holding the
            // mutex so that concurrent requests are not blocked.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;

            // Re-acquire the lock just for the post-init feed / pgid check.
            {
                let mut s = state.lock().await;
                if let Some(session) = s.sessions.get_mut(&id) {
                    session.feed();
                    session.check_fg_pgid();
                }
            }

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
/// Returns error string on failure so callers can decide whether to proceed.
fn set_nonblocking(fd: Option<std::os::unix::io::RawFd>) -> Result<(), String> {
    if let Some(fd) = fd {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 {
                return Err(format!("fcntl F_GETFL failed for fd {}: {}", fd, std::io::Error::last_os_error()));
            }
            if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(format!("fcntl F_SETFL failed for fd {}: {}", fd, std::io::Error::last_os_error()));
            }
        }
    }
    Ok(())
}

async fn handle_destroy(state: Arc<Mutex<AppState>>, session_id: String) -> Response {
    // Step 1: mark destroying + kill + close recording under lock
    {
        let mut s = state.lock().await;
        match s.sessions.get_mut(&session_id) {
            Some(session) => {
                session.destroying = true;
                session.kill();
                session.close_recording();
            }
            None => return Response::err("session not found"),
        }
    }

    // Step 2: sleep WITHOUT lock to let SIGHUP take effect
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Step 3: force kill + reap + remove under lock
    let mut s = state.lock().await;
    match s.sessions.get_mut(&session_id) {
        Some(session) => {
            session.force_kill(); // SIGKILL + reap
        }
        None => return Response::err("session not found"),
    }
    s.sessions.remove(&session_id); // Drop triggers Session::drop -> try_wait
    Response {
        ok: true,
        session_id: Some(session_id),
        ..Response::ok()
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

        if session.destroying {
            return Response::err("session is being destroyed");
        }

        if let Some(exit_code) = session.exited {
            return Response {
                ok: false,
                error: Some("session exited".into()),
                exit_code: Some(exit_code),
                ..Response::ok()
            };
        }

        seq = session.next_seq();
        write_cursor_before = session.ringbuf.write_cursor();

        if let Some(ref ctrl) = ctrl {
            if !text.is_empty() {
                return Response::err("cannot specify both --ctrl and text");
            }
            if let Err(e) = session.send_ctrl(ctrl) {
                return Response::err(e);
            }
        } else if !text.is_empty() {
            if let Err(e) = session.send_text(&text) {
                return Response::err(e);
            }
        } else {
            // No text and no ctrl — nothing to send, return immediately
            return Response {
                ok: true,
                seq: Some(seq),
                output: Some(String::new()),
                elapsed_ms: Some(0),
                ..Response::ok()
            };
        }

        // Drain any immediate output
        session.feed();
    }

    // Step 2: If nowait or ctrl, return immediately
    if nowait || ctrl.is_some() {
        let mut s = state.lock().await;
        let session = match s.sessions.get_mut(&session_id) {
            Some(s) => s,
            None => return Response::err("session not found"),
        };
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
    //
    // Track overflow per-send. take_overflow() resets the flag,
    // so any overflow detected in the loop must have happened during this send.
    {
        let mut s = state.lock().await;
        if let Some(session) = s.sessions.get_mut(&session_id) {
            let _ = session.ringbuf.take_overflow(); // clear any pre-existing overflow
        }
    }

    let deadline = start + std::time::Duration::from_millis(timeout_ms);
    let mut stable_since: Option<std::time::Instant> = None;
    let mut last_wc = write_cursor_before;

    loop {
        if std::time::Instant::now() >= deadline {
            let mut s = state.lock().await;
            let session = match s.sessions.get_mut(&session_id) {
                Some(s) => s,
                None => return Response::err("session not found"),
            };
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

            // Check prompt regex match in all output since this send started.
            // We read from write_cursor_before (not prompt_check_cursor) to
            // ensure prompts that arrive split across multiple PTY reads are
            // still matched correctly. The allocation is bounded by buffer size
            // and only occurs when a prompt regex is configured.
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
            let session = match s.sessions.get_mut(&session_id) {
                Some(s) => s,
                None => return Response::err("session not found"),
            };
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
    let session = match state.sessions.get(session_id) {
        Some(s) => s,
        None => return (Vec::new(), false, 0),
    };

    // Only track client cursor if a client_id is provided
    if let Some(ref cid) = client_id {
        let cursor = state.clients.get(cid).map(|c| c.read_cursor).unwrap_or(0);
        let (data, gap, lost) = session.ringbuf.read(cursor);
        let new_cursor = session.ringbuf.write_cursor();
        state.clients.insert(
            cid.clone(),
            ClientState {
                read_cursor: new_cursor,
                last_active: std::time::Instant::now(),
            },
        );
        (data, gap, lost)
    } else {
        // No client_id: read all output from cursor 0 (full buffer)
        let (data, gap, lost) = session.ringbuf.read(0);
        (data, gap, lost)
    }
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
        session.feed(); // Drain PTY output first so terminal state is up-to-date
        let screen_data = session.term.screen();
        let cursor = session.term.cursor();
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

    // Drain PTY in both paths (running and exited) to ensure the
    // ring buffer reflects all available output before we read it.
    session.feed();

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
        return resp;
    }
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
            created_at: session.created_at,
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

async fn handle_mouse(
    state: Arc<Mutex<AppState>>,
    session_id: String,
    action: String,
    x: u16,
    y: u16,
    button: Option<String>,
    direction: Option<String>,
    count: Option<u16>,
    to_x: Option<u16>,
    to_y: Option<u16>,
    steps: Option<u16>,
) -> Response {
    use agent_shell_core::mouse;

    let mut s = state.lock().await;
    let session = match s.sessions.get_mut(&session_id) {
        Some(s) => s,
        None => return Response::err("session not found"),
    };

    if session.destroying {
        return Response::err("session is being destroyed");
    }

    if session.exited.is_some() {
        return Response::err("session exited");
    }

    // Validate coordinates
    if x == 0 || y == 0 {
        return Response::err("coordinates must be >= 1 (1-based)");
    }
    if x > session.cols {
        return Response::err(format!(
            "x coordinate {} exceeds terminal width {}",
            x, session.cols
        ));
    }
    if y > session.rows {
        return Response::err(format!(
            "y coordinate {} exceeds terminal height {}",
            y, session.rows
        ));
    }

    let button_str = button.as_deref().unwrap_or("left");
    let btn = match mouse::parse_button(button_str) {
        Ok(b) => b,
        Err(e) => return Response::err(e),
    };

    let count = count.unwrap_or(1);
    if count > 100 {
        return Response::err("count must be <= 100");
    }
    if count == 0 {
        return Response::err("count must be >= 1");
    }

    let sequences: Vec<Vec<u8>> = match action.as_str() {
        "click" => mouse::encode_click(btn, x, y, count),
        "scroll" => {
            let dir_str = match direction.as_deref() {
                Some(d) => d,
                None => return Response::err("scroll requires --direction (up|down)"),
            };
            let dir = match mouse::parse_direction(dir_str) {
                Ok(d) => d,
                Err(e) => return Response::err(e),
            };
            mouse::encode_scroll(dir, x, y, count)
        }
        "press" => vec![mouse::encode_press(btn, x, y)],
        "release" => vec![mouse::encode_release(btn, x, y)],
        "move" => vec![mouse::encode_move(btn, x, y)],
        "drag" => {
            let tx = match to_x {
                Some(v) if v >= 1 && v <= session.cols => v,
                Some(v) if v == 0 => return Response::err("to_x must be >= 1"),
                Some(v) => {
                    return Response::err(format!(
                        "to_x coordinate {} exceeds terminal width {}",
                        v, session.cols
                    ))
                }
                None => return Response::err("drag requires --to-x"),
            };
            let ty = match to_y {
                Some(v) if v >= 1 && v <= session.rows => v,
                Some(v) if v == 0 => return Response::err("to_y must be >= 1"),
                Some(v) => {
                    return Response::err(format!(
                        "to_y coordinate {} exceeds terminal height {}",
                        v, session.rows
                    ))
                }
                None => return Response::err("drag requires --to-y"),
            };
            let steps = steps.unwrap_or(5);
            if steps > 100 {
                return Response::err("steps must be <= 100");
            }
            mouse::encode_drag(btn, x, y, tx, ty, steps)
        }
        other => return Response::err(format!("unknown mouse action: '{}'", other)),
    };

    // Write all sequences to PTY
    for seq in &sequences {
        if let Err(e) = session.send_raw_bytes(seq) {
            return Response::err(e);
        }
    }

    Response {
        ok: true,
        session_id: Some(session_id),
        ..Response::ok()
    }
}

async fn handle_stop(state: Arc<Mutex<AppState>>) {
    // Step 1: kill all sessions under lock
    {
        let mut s = state.lock().await;
        for session in s.sessions.values_mut() {
            session.kill();
            session.close_recording();
        }
    }

    // Step 2: sleep WITHOUT lock to let SIGHUP take effect
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Step 3: force kill + reap + remove all under lock
    {
        let mut s = state.lock().await;
        for session in s.sessions.values_mut() {
            session.force_kill(); // SIGKILL + reap
        }
        s.sessions.clear(); // Drop all sessions -> Session::drop -> try_wait
    }

    // Clean up and exit directly (no response needed — we're shutting down)
    let socket_path = Config::base_dir().join("daemon.sock");
    let pid_path = Config::base_dir().join("daemon.pid");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&pid_path);

    // Give the response a brief moment to be sent, then exit.
    // The response was already written by send_response before this was called.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    std::process::exit(0);
}

/// Event from a read-with-timeout operation.
enum StreamEvent<'a> {
    Data(&'a [u8]),
    Eof,
    Timeout,
}

/// Read from stream_rx with a timeout. Used in readonly attach mode
/// where we need to simultaneously wait for both daemon output (via
/// ringbuf poll) and stdin exit keys.
async fn stdin_read_with_timeout<'a>(
    stream_rx: &mut tokio::net::unix::OwnedReadHalf,
    buf: &'a mut [u8],
    timeout: std::time::Duration,
) -> StreamEvent<'a> {
    tokio::select! {
        result = stream_rx.read(buf) => {
            match result {
                Ok(0) => StreamEvent::Eof,
                Ok(n) => StreamEvent::Data(&buf[..n]),
                Err(_) => StreamEvent::Eof,
            }
        }
        _ = tokio::time::sleep(timeout) => StreamEvent::Timeout,
    }
}

async fn handle_attach(
    mut stream: tokio::net::UnixStream,
    state: Arc<Mutex<AppState>>,
    session_id: String,
    readonly: bool,
) {
    // Grab the shared PTY-output notifier before entering the loop.
    let pty_notify: Arc<Notify> = {
        let s = state.lock().await;
        s.pty_output_notify.clone()
    };
    // ── Phase 1: validate & send initial JSON handshake ──────────────────
    // Generate a full ANSI redraw from the terminal emulator state.
    // This correctly handles alternate screen, SGR attributes, and avoids
    // the ringbuf-overflow problem where raw bytes could be truncated.
    let (redraw_output, start_cursor, already_exited) = {
        let mut s = state.lock().await;
        let session = match s.sessions.get_mut(&session_id) {
            Some(s) => s,
            None => {
                send_response(&mut stream, &Response::err("session not found")).await;
                return;
            }
        };
        if session.destroying {
            send_response(&mut stream, &Response::err("session is being destroyed")).await;
            return;
        }
        // Allow attaching to exited sessions: drain whatever is in the ringbuf
        // and immediately close. This handles short-lived programs (ls, echo, etc.)
        // that finish before the attach handshake arrives.
        let already_exited = session.exited.is_some();
        session.feed();
        let wc = session.ringbuf.write_cursor();
        let redraw = session.term.full_redraw();
        (redraw, wc, already_exited)
    };

    // JSON response carries the terminal redraw output.
    // We use base64 encoding to safely transport binary data through JSON
    // without corrupting escape sequences via lossy UTF-8 conversion.
    let init_resp = Response {
        ok: true,
        output: Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &redraw_output,
        )),
        ..Response::ok()
    };
    send_response(&mut stream, &init_resp).await;

    // Short-lived programs (ls, echo, etc.) may have already exited by the time
    // we reach here. The ringbuf data was sent in the handshake above; nothing
    // more to stream.
    if already_exited {
        return;
    }

    // ── Phase 2: raw binary bidirectional streaming ─────────────────────
    // We do NOT use AsyncFd on the PTY master fd. The PTY fd lifetime is
    // tied to the Session, and if the session is destroyed while attach is
    // running, the fd could be closed/reused under us. Instead, we poll the
    // ringbuf at 50ms intervals. The reaper task and send handler call feed()
    // to drain PTY output into the ringbuf; we just read from there.
    // This eliminates the fd reuse race entirely.

    let (mut stream_rx, mut stream_tx) = stream.into_split();
    let mut pty_cursor = start_cursor;
    let mut running = true;

    while running {
        let mut stdin_buf = [0u8; 4096];

        if readonly {
            // ── readonly: wake on PTY output or stdin exit key ──
            // Use pty_notify so new PTY output is forwarded immediately instead
            // of waiting up to 50 ms for a timer tick.
            tokio::select! {
                // PTY has new output → fall through to ringbuf drain below
                _ = pty_notify.notified() => {}
                // stdin → check for exit keys only (50 ms timeout as safety net)
                result = stdin_read_with_timeout(&mut stream_rx, &mut stdin_buf,
                                                 std::time::Duration::from_millis(50)) => {
                    match result {
                        StreamEvent::Data(data) => {
                            if data.contains(&0x03) || data.contains(&0x04) {
                                running = false;
                            }
                        }
                        StreamEvent::Eof => running = false,
                        StreamEvent::Timeout => {}
                    }
                }
            }
        } else {
            // ── rw: wake on client keystroke OR PTY output ──
            // The third select! arm (pty_notify) ensures PTY→client latency is
            // not bounded by the 50 ms poll interval. This is critical for
            // round-trip sequences like ESC[6n → CPR that vim uses to detect
            // terminal capabilities during startup (ttimeoutlen = 100 ms).
            tokio::select! {
                // client keystroke → PTY
                result = stream_rx.read(&mut stdin_buf) => {
                    match result {
                        Ok(0) => { running = false; continue; }
                        Ok(n) => {
                            let data = &stdin_buf[..n];
                            // Ctrl-C (0x03) detaches without forwarding to PTY
                            if data.contains(&0x03) {
                                running = false;
                                continue;
                            }
                            let mut s = state.lock().await;
                            if let Some(session) = s.sessions.get_mut(&session_id) {
                                if session.destroying {
                                    running = false;
                                    continue;
                                }
                                match session.pty_writer.lock() {
                                    Ok(ref mut writer) => {
                                        let _ = writer.write_all(data);
                                        let _ = writer.flush();
                                    }
                                    Err(e) => {
                                        eprintln!("attach: pty_writer lock poisoned: {}", e);
                                        running = false;
                                    }
                                }
                                if let Some(ref mut rec) = session.recording {
                                    rec.record_in(data);
                                }
                            } else {
                                running = false;
                            }
                        }
                        Err(_) => { running = false; continue; }
                    }
                }
                // PTY has new output → fall through to ringbuf drain below
                _ = pty_notify.notified() => {}
                // Safety-net fallback: drain even without a notification
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
        }

        // ── drain ringbuf output → client ──
        let (data, exited, gone) = {
            let mut s = state.lock().await;
            let notify = s.pty_output_notify.clone();
            match s.sessions.get_mut(&session_id) {
                Some(session) => {
                    // Feed PTY→ringbuf; if new bytes arrived, wake other attach loops.
                    let n = session.feed();
                    let exited = session.exited.is_some();
                    let wc = session.ringbuf.write_cursor();
                    let data = if wc > pty_cursor {
                        let (d, _, _) = session.ringbuf.read(pty_cursor);
                        pty_cursor = wc;
                        d
                    } else {
                        Vec::new()
                    };
                    if n > 0 { notify.notify_waiters(); }
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
        if gone || exited {
            running = false;
        }
    }
}
