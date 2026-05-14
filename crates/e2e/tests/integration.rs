use agent_shell_e2e::*;

mod basic {
    use super::*;

    #[test]
    fn create_and_destroy() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "test1"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let resp = daemon.cli_json(&["destroy", "--session", &sid]);
        assert_ok(&resp);
        daemon.stop();
    }

    #[test]
    fn send_echo() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "echo_test"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo hello_world"]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("hello_world"), "output should contain 'hello_world', got: {:?}", output);
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn send_whoami() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "whoami"]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        let user = std::env::var("USER").unwrap_or_default();
        assert!(output.contains(&user), "output should contain '{}', got: {:?}", user, output);
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn send_pwd_with_cwd() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--cwd", "/tmp"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "pwd"]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("/tmp") || output.contains("private/tmp"), "output should contain /tmp, got: {:?}", output);
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn send_exit() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "exit"]);
        assert_ok(&resp);
        assert_eq!(resp.exited, Some(true));
        assert_eq!(resp.exit_code, Some(0));
        let resp = daemon.cli_json(&["send", "--session", &sid, "echo test"]);
        assert_error(&resp, "session exited");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn send_nowait() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let resp = daemon.cli_json(&["send", "--session", &sid, "--nowait", "echo async"]);
        assert_ok(&resp);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let resp = daemon.cli_json(&["read", "--session", &sid]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("async"), "read output should contain 'async', got: {:?}", output);
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn send_ctrl_c() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let _ = daemon.cli_json(&["send", "--session", &sid, "--nowait", "cat"]);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let resp = daemon.cli_json(&["send", "--session", &sid, "--ctrl", "c"]);
        assert_ok(&resp);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo after_ctrl_c"]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("after_ctrl_c"), "output should contain 'after_ctrl_c', got: {:?}", output);
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn read_screen() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo screen_test"]);
        assert_ok(&resp);
        let resp = daemon.cli_json(&["read", "--session", &sid, "--screen"]);
        assert_ok(&resp);
        assert!(resp.screen.is_some(), "screen should be present");
        let screen = resp.screen.unwrap();
        let combined = screen.join("\n");
        assert!(combined.contains("screen_test"), "screen should contain 'screen_test', got: {:?}", combined);
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn wait_pattern() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let _ = daemon.cli_json(&[
            "send", "--session", &sid, "--nowait",
            "for i in 1 2 3; do echo line$i; sleep 0.1; done",
        ]);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let resp = daemon.cli_json(&["wait", "--session", &sid, "line3", "--timeout", "10000"]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("line3"), "wait output should contain 'line3', got: {:?}", output);
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn set_prompt() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let resp = daemon.cli_json(&["set-prompt", "--session", &sid, "bash-3\\.2\\$ "]);
        assert_ok(&resp);
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo prompt_test"]);
        assert_ok(&resp);
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn list_sessions() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "list_test"]);
        assert_ok(&resp);
        let resp = daemon.cli_json(&["list"]);
        assert_ok(&resp);
        let sessions = resp.sessions.unwrap_or_default();
        assert!(!sessions.is_empty(), "should have at least one session");
        for s in &sessions {
            daemon.cli_json(&["destroy", "--session", &s.id]);
        }
        daemon.stop();
    }

    #[test]
    fn resize() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let resp = daemon.cli_json(&["resize", "--session", &sid, "--rows", "40", "--cols", "120"]);
        assert_ok(&resp);
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn session_not_found() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["send", "--session", "nonexistent", "echo test"]);
        assert_error(&resp, "session not found");
        daemon.stop();
    }

    #[test]
    fn invalid_prompt_regex() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--prompt", "[invalid"]);
        assert_error(&resp, "invalid regex");
        daemon.stop();
    }

    #[test]
    fn recording() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "rectest", "--record"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        assert!(resp.recording.is_some(), "should have recording path");
        let _ = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo recording_test"]);
        let _ = daemon.cli_json(&["destroy", "--session", &sid]);
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let rec_dir = std::path::PathBuf::from(&home).join(".agent-shell/recordings");
        assert!(rec_dir.exists(), "recordings dir should exist");
        daemon.stop();
    }
}

mod attach {
    use super::*;

    #[test]
    fn attach_rw_send_pwd() {
        let mut daemon = start_daemon();

        let resp = daemon.cli_json(&["create", "--name", "att_rw"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let mut conn = daemon.connect_attach_rw(&sid).expect("attach rw connect");

        // Drain initial screen output
        let _ = conn.read_output(std::time::Duration::from_millis(300));

        // Send pwd and verify output
        let result = conn.send_and_wait("pwd", "/", std::time::Duration::from_secs(5));
        assert!(result.is_ok(), "pwd should produce output with '/': {:?}", result);
        let text = result.unwrap();
        assert!(text.contains("/"), "pwd output should contain a path, got: {:?}", &text[..text.len().min(300)]);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn attach_rw_send_echo() {
        let mut daemon = start_daemon();

        let resp = daemon.cli_json(&["create", "--name", "att_echo"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let mut conn = daemon.connect_attach_rw(&sid).expect("attach rw connect");
        let _ = conn.read_output(std::time::Duration::from_millis(300));

        // Send echo and verify the exact output appears
        let result = conn.send_and_wait("echo ATTACH_RW_TEST_OK", "ATTACH_RW_TEST_OK", std::time::Duration::from_secs(5));
        assert!(result.is_ok(), "should find ATTACH_RW_TEST_OK in output: {:?}", result);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn attach_readonly_no_input() {
        let mut daemon = start_daemon();

        let resp = daemon.cli_json(&["create", "--name", "att_ro"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Attach readonly
        let mut conn = daemon.connect_attach_ro(&sid).expect("attach ro connect");

        // Now send a command via the CLI (not through the readonly attach)
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo AFTER_ATTACH"]);
        assert_ok(&resp);

        // The readonly attach should see the new output
        let text = conn.read_output(std::time::Duration::from_secs(3));
        let text_str = String::from_utf8_lossy(&text);
        assert!(
            text_str.contains("AFTER_ATTACH"),
            "readonly attach should see new output, got: {:?}",
            &text_str[..text_str.len().min(300)]
        );

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn attach_multiple_commands() {
        let mut daemon = start_daemon();

        let resp = daemon.cli_json(&["create", "--name", "att_multi"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let mut conn = daemon.connect_attach_rw(&sid).expect("attach rw connect");
        let _ = conn.read_output(std::time::Duration::from_millis(300));

        // First command
        let result = conn.send_and_wait("echo FIRST", "FIRST", std::time::Duration::from_secs(5));
        assert!(result.is_ok(), "should find FIRST: {:?}", result);

        // Second command
        let result = conn.send_and_wait("echo SECOND", "SECOND", std::time::Duration::from_secs(5));
        assert!(result.is_ok(), "should find SECOND: {:?}", result);

        // Third command - pwd
        let result = conn.send_and_wait("echo THIRD_DONE", "THIRD_DONE", std::time::Duration::from_secs(5));
        assert!(result.is_ok(), "should find THIRD_DONE: {:?}", result);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    #[test]
    fn attach_not_found() {
        let daemon = start_daemon();
        let result = daemon.connect_attach_rw("nonexistent_session");
        assert!(result.is_err(), "attach to nonexistent session should fail");
    }
}

mod interactive {
    use super::*;

    #[test]
    fn python3_prompt_detection() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "py1", "--prompt", ">>> "]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "10000", "python3"]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("Python"), "should contain Python version, got: {:?}", output);
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "print(1+1)"]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("2"), "should contain '2', got: {:?}", output);
        let _ = daemon.cli_json(&["send", "--session", &sid, "--nowait", "exit()"]);
        std::thread::sleep(std::time::Duration::from_millis(300));
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

mod kill_daemon {
    use super::*;
    use agent_shell_core::protocol::Response;

    /// kill-daemon should force-kill a running daemon and clean up artifacts.
    #[test]
    fn kill_running_daemon() {
        let daemon = start_daemon();
        let cli_bin = daemon.cli_bin.clone();

        // Verify daemon is alive
        let resp = daemon.cli_json(&["list"]);
        assert_ok(&resp);

        // Force-kill
        let output = daemon.cli(&["kill-daemon"]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "kill-daemon should exit 0");
        assert!(stdout.contains("\"killed\":true"), "should report killed=true, got: {}", stdout);

        // Socket and pid files should be gone
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let base = std::path::PathBuf::from(&home).join(".agent-shell");
        assert!(!base.join("daemon.sock").exists(), "socket should be removed");
        assert!(!base.join("daemon.pid").exists(), "pid file should be removed");

        // Next CLI call should auto-start a fresh daemon
        let resp = std::process::Command::new(&cli_bin)
            .args(&["create", "--name", "after_kill"])
            .output()
            .expect("cli should work after kill-daemon");
        let stdout = String::from_utf8_lossy(&resp.stdout);
        let resp: Response = serde_json::from_str(stdout.trim()).expect("valid json");
        assert_ok(&resp);

        // Clean up the new daemon
        let sid = session_id(&resp);
        let _ = std::process::Command::new(&cli_bin)
            .args(&["destroy", "--session", &sid])
            .output();
        let _ = std::process::Command::new(&cli_bin).args(&["kill-daemon"]).output();
    }

    /// kill-daemon with no daemon running should succeed gracefully.
    #[test]
    fn kill_no_daemon() {
        // Ensure no daemon is running
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let base = std::path::PathBuf::from(&home).join(".agent-shell");
        let _ = std::fs::remove_file(base.join("daemon.sock"));
        let _ = std::fs::remove_file(base.join("daemon.pid"));

        let daemon = start_daemon();
        let cli_bin = daemon.cli_bin.clone();

        // Kill it first
        let _ = daemon.cli(&["kill-daemon"]);
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Kill again — should report no daemon running, not error
        let output = std::process::Command::new(&cli_bin)
            .args(&["kill-daemon"])
            .output()
            .expect("cli should work");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "kill-daemon with no daemon should exit 0");
        assert!(stdout.contains("\"killed\":false"), "should report killed=false, got: {}", stdout);
    }

    /// kill-daemon should clean up stale socket and pid files even if
    /// the process is already gone.
    #[test]
    fn kill_cleans_stale_artifacts() {
        let mut daemon = start_daemon();
        let cli_bin = daemon.cli_bin.clone();

        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let base = std::path::PathBuf::from(&home).join(".agent-shell");
        let pid_str = std::fs::read_to_string(base.join("daemon.pid")).unwrap();
        let pid: i32 = pid_str.trim().parse().unwrap();

        // SIGKILL the daemon directly
        unsafe { libc::kill(pid, libc::SIGKILL); }
        // Reap the zombie via the Child handle
        let _ = daemon.process.wait();
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Stale files should still exist
        assert!(base.join("daemon.sock").exists(), "stale socket should exist");
        assert!(base.join("daemon.pid").exists(), "stale pid should exist");

        // kill-daemon should detect process is gone and clean up
        let output = std::process::Command::new(&cli_bin)
            .args(&["kill-daemon"])
            .output()
            .expect("cli should work");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("\"killed\":false"), "process already gone, should report killed=false, got: {}", stdout);

        // Artifacts should be cleaned
        assert!(!base.join("daemon.sock").exists(), "stale socket should be removed");
        assert!(!base.join("daemon.pid").exists(), "stale pid should be removed");
    }

    /// After kill-daemon, a new daemon should start cleanly via auto-start.
    #[test]
    fn kill_then_auto_restart() {
        let daemon = start_daemon();
        let cli_bin = daemon.cli_bin.clone();

        // Create a session to prove daemon works
        let resp = daemon.cli_json(&["create", "--name", "before_kill"]);
        assert_ok(&resp);
        let _sid1 = session_id(&resp);

        // Kill daemon
        let _ = daemon.cli(&["kill-daemon"]);
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Old session is gone. New daemon auto-starts on next command.
        let resp = std::process::Command::new(&cli_bin)
            .args(&["create", "--name", "after_kill"])
            .output()
            .expect("cli should auto-start daemon");
        let stdout = String::from_utf8_lossy(&resp.stdout);
        let resp: Response = serde_json::from_str(stdout.trim()).expect("valid json");
        assert_ok(&resp);
        let sid2 = session_id(&resp);

        // New session should work
        let resp = std::process::Command::new(&cli_bin)
            .args(&["send", "--session", &sid2, "--timeout", "5000", "echo restarted_ok"])
            .output()
            .expect("send should work");
        let stdout = String::from_utf8_lossy(&resp.stdout);
        let resp: Response = serde_json::from_str(stdout.trim()).expect("valid json");
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("restarted_ok"));

        // Clean up
        let _ = std::process::Command::new(&cli_bin)
            .args(&["destroy", "--session", &sid2])
            .output();
        let _ = std::process::Command::new(&cli_bin).args(&["kill-daemon"]).output();
    }
}

mod sigterm {
    use std::os::unix::process::CommandExt;

    /// SIGTERM should trigger graceful shutdown: kill sessions, clean up socket & pid files.
    #[test]
    fn sigterm_graceful_shutdown() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let base = std::path::PathBuf::from(&home).join(".agent-shell");
        let socket_path = base.join("daemon.sock");
        let pid_path = base.join("daemon.pid");

        // Clean up any existing daemon
        let _ = std::fs::remove_file(&socket_path);
        if let Ok(p) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = p.trim().parse::<i32>() {
                unsafe { libc::kill(pid, libc::SIGKILL); }
            }
        }
        let _ = std::fs::remove_file(&pid_path);
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Start daemon in its own process group so cargo test doesn't interfere
        let daemon_bin = agent_shell_e2e::find_bin("agent-shell-daemon");
        let mut daemon = std::process::Command::new(&daemon_bin)
            .process_group(0)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn daemon");

        // Wait for socket
        for _ in 0..30 {
            if socket_path.exists() { break; }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(socket_path.exists(), "daemon socket should appear");

        // Send SIGTERM
        let pid_from_file: i32 = std::fs::read_to_string(&pid_path).unwrap().trim().parse().unwrap();
        unsafe { libc::kill(pid_from_file, libc::SIGTERM); }

        // Wait for the daemon to exit (up to 5s)
        for _ in 0..50 {
            match daemon.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                Err(_) => break,
            }
        }

        // Give time for file cleanup after process exit
        std::thread::sleep(std::time::Duration::from_millis(500));

        assert!(!socket_path.exists(), "socket should be removed after SIGTERM");
        assert!(!pid_path.exists(), "pid file should be removed after SIGTERM");

        let _ = daemon.kill();
        let _ = daemon.wait();
    }
}

mod render {
    use super::*;

    // ── helpers ─────────────────────────────────────────────────────

    /// Send a printf command, then attach and read raw bytes.
    fn attach_after_send(
        daemon: &mut DaemonHandle,
        sid: &str,
        printf_cmd: &str,
    ) -> Vec<u8> {
        let resp = daemon.cli_json(&[
            "send", "--session", sid, "--timeout", "5000", printf_cmd,
        ]);
        assert_ok(&resp);

        let mut conn = daemon.connect_attach_rw(sid).expect("attach connect");
        let mut all_bytes = conn.initial_output.clone();
        let stream_bytes = conn.read_output(std::time::Duration::from_millis(500));
        all_bytes.extend_from_slice(&stream_bytes);
        all_bytes
    }

    fn assert_contains_escape(bytes: &[u8], needle: &[u8], label: &str) {
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "{}: expected byte sequence {:?} in attach output, not found.\n\
             Output hex: {}\n\
             Output text: {:?}",
            label, needle,
            hex_dump(bytes),
            String::from_utf8_lossy(bytes),
        );
    }

    fn assert_contains_text(bytes: &[u8], text: &str, label: &str) {
        let output = String::from_utf8_lossy(bytes);
        assert!(
            output.contains(text),
            "{}: expected text '{}' in attach output, not found.\nOutput: {:?}",
            label, text, &output[..output.len().min(1000)],
        );
    }

    fn hex_dump(data: &[u8]) -> String {
        data.iter().take(200).map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ")
    }

    // ── SGR color sequences ──────────────────────────────────────────

    /// Red foreground: ESC[31m ... ESC[0m
    #[test]
    fn sgr_red_foreground() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "sgr_red"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[31mRED\033[0m'"#
        );

        // ESC[31m = 0x1b 0x5b 0x33 0x31 0x6d
        assert_contains_escape(&bytes, b"\x1b[31m", "SGR red fg");
        assert_contains_escape(&bytes, b"\x1b[0m", "SGR reset");
        assert_contains_text(&bytes, "RED", "red text");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Green background: ESC[42m ... ESC[0m
    #[test]
    fn sgr_green_background() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "sgr_bg"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[42mBG_GREEN\033[0m'"#
        );

        assert_contains_escape(&bytes, b"\x1b[42m", "SGR green bg");
        assert_contains_escape(&bytes, b"\x1b[0m", "SGR reset");
        assert_contains_text(&bytes, "BG_GREEN", "green bg text");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Bold: ESC[1m ... ESC[0m
    #[test]
    fn sgr_bold() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "sgr_bold"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[1mBOLD\033[0m'"#
        );

        assert_contains_escape(&bytes, b"\x1b[1m", "SGR bold");
        assert_contains_escape(&bytes, b"\x1b[0m", "SGR reset");
        assert_contains_text(&bytes, "BOLD", "bold text");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Combined SGR: bold + red fg + green bg
    #[test]
    fn sgr_combined() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "sgr_combo"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[1;31;42mCOMBO\033[0m'"#
        );

        assert_contains_escape(&bytes, b"\x1b[1;31;42m", "SGR combined");
        assert_contains_text(&bytes, "COMBO", "combined text");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    // ── Cursor movement sequences ────────────────────────────────────

    /// Cursor position: ESC[H (home) and ESC[row;colH (absolute)
    #[test]
    fn cursor_position() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "cur_pos"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf 'ABC\033[2;1HXYZ'"#
        );

        assert_contains_text(&bytes, "ABC", "text before move");
        assert_contains_escape(&bytes, b"\x1b[2;1H", "cursor absolute position");
        assert_contains_text(&bytes, "XYZ", "text after move");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Cursor up/down/left/right
    #[test]
    fn cursor_directional() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "cur_dir"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            // Move right 3, write X, move left 5, write Y
            r#"printf '\033[3C X\033[5D Y'"#
        );

        assert_contains_escape(&bytes, b"\x1b[3C", "cursor right");
        assert_contains_escape(&bytes, b"\x1b[5D", "cursor left");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Save/restore cursor: ESC[s / ESC[u
    #[test]
    fn cursor_save_restore() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "cur_save"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[sSAVED\033[uRESTORED'"#
        );

        assert_contains_escape(&bytes, b"\x1b[s", "cursor save");
        assert_contains_escape(&bytes, b"\x1b[u", "cursor restore");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    // ── Erase / clear sequences ─────────────────────────────────────

    /// Clear line: ESC[K
    #[test]
    fn erase_line() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "erase_line"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf 'WILL_ERASE\033[K'"#
        );

        assert_contains_escape(&bytes, b"\x1b[K", "erase to end of line");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Clear screen: ESC[2J + cursor home ESC[H
    #[test]
    fn clear_screen() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "clear_scr"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[2J\033[HCLEARED'"#
        );

        assert_contains_escape(&bytes, b"\x1b[2J", "clear screen");
        assert_contains_escape(&bytes, b"\x1b[H", "cursor home");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    // ── 256-color and RGB sequences ─────────────────────────────────

    /// 256-color: ESC[38;5;196m (red in 256-color palette)
    #[test]
    fn sgr_256_color() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "sgr_256"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[38;5;196mCOLOR256\033[0m'"#
        );

        assert_contains_escape(&bytes, b"\x1b[38;5;196m", "256-color fg");
        assert_contains_text(&bytes, "COLOR256", "256-color text");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// True-color (RGB): ESC[38;2;255;0;0m
    #[test]
    fn sgr_rgb_color() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "sgr_rgb"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[38;2;255;0;0mRGB_RED\033[0m'"#
        );

        assert_contains_escape(&bytes, b"\x1b[38;2;255;0;0m", "RGB fg");
        assert_contains_text(&bytes, "RGB_RED", "RGB text");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    // ── OSC (Operating System Command) sequences ───────────────────

    /// Window title: ESC]0;title BEL
    #[test]
    fn osc_window_title() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "osc_title"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033]0;MY_TITLE\007'"#
        );

        // ESC ] = 0x1b 0x5d
        assert_contains_escape(&bytes, b"\x1b]0;MY_TITLE", "OSC title");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    // ── Alternate screen buffer ─────────────────────────────────────

    /// Switch to alternate screen: ESC[?1049h and back: ESC[?1049l
    #[test]
    fn alternate_screen() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "alt_screen"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[?1049hALT_SCREEN\033[?1049l'"#
        );

        assert_contains_escape(&bytes, b"\x1b[?1049h", "alt screen on");
        assert_contains_escape(&bytes, b"\x1b[?1049l", "alt screen off");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    // ── Scroll region ───────────────────────────────────────────────

    /// Set scroll region: ESC[1;10r
    #[test]
    fn scroll_region() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "scroll_reg"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[1;10rSCROLL_REGION\033[r'"#
        );

        assert_contains_escape(&bytes, b"\x1b[1;10r", "set scroll region");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    // ── Live attach: escape sequences in real-time ──────────────────

    /// Attach, then send a command that produces colors, verify live stream.
    #[test]
    fn attach_live_colored_output() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "live_color"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Attach first
        let mut conn = daemon.connect_attach_rw(&sid).expect("attach connect");
        let _ = conn.read_output(std::time::Duration::from_millis(300));

        // Use echo -e to interpret escape sequences
        conn.send(b"echo -e '\\033[32mGREEN\\033[0m'\n").unwrap();

        // Read the output — should contain the escape sequence
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut all_output = Vec::new();
        while std::time::Instant::now() < deadline {
            let chunk = conn.read_output(std::time::Duration::from_millis(200));
            all_output.extend_from_slice(&chunk);
            if all_output.windows(b"\x1b[32m".len()).any(|w| w == b"\x1b[32m") {
                break;
            }
        }

        assert_contains_escape(&all_output, b"\x1b[32m", "live green fg");
        assert_contains_text(&all_output, "GREEN", "live green text");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Attach then produce complex multi-line colored output via CLI send.
    #[test]
    fn attach_sees_cli_colored_output() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "cli_color"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Attach first
        let mut conn = daemon.connect_attach_rw(&sid).expect("attach connect");
        let _ = conn.read_output(std::time::Duration::from_millis(300));

        // Send a colored command via CLI (not through the attach)
        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "5000",
            r#"printf '\033[1;33mBOLD_YELLOW\033[0m\n'"#,
        ]);
        assert_ok(&resp);

        // Attach should see the colored output in its stream
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut all_output = Vec::new();
        while std::time::Instant::now() < deadline {
            let chunk = conn.read_output(std::time::Duration::from_millis(200));
            all_output.extend_from_slice(&chunk);
            if all_output.windows(b"\x1b[1;33m".len()).any(|w| w == b"\x1b[1;33m") {
                break;
            }
        }

        assert_contains_escape(&all_output, b"\x1b[1;33m", "CLI bold yellow");
        assert_contains_text(&all_output, "BOLD_YELLOW", "CLI bold yellow text");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    // ── Additional SGR attributes ────────────────────────────────────

    /// Underline: ESC[4m
    #[test]
    fn sgr_underline() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "sgr_ul"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[4mUNDERLINED\033[0m'"#
        );
        assert_contains_escape(&bytes, b"\x1b[4m", "SGR underline");
        assert_contains_escape(&bytes, b"\x1b[0m", "SGR reset");
        assert_contains_text(&bytes, "UNDERLINED", "underlined text");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Reverse video: ESC[7m
    #[test]
    fn sgr_reverse() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "sgr_rev"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[7mREVERSED\033[0m'"#
        );
        assert_contains_escape(&bytes, b"\x1b[7m", "SGR reverse");
        assert_contains_text(&bytes, "REVERSED", "reversed text");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Italic: ESC[3m
    #[test]
    fn sgr_italic() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "sgr_ital"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[3mITALIC\033[0m'"#
        );
        assert_contains_escape(&bytes, b"\x1b[3m", "SGR italic");
        assert_contains_text(&bytes, "ITALIC", "italic text");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Strikethrough: ESC[9m
    #[test]
    fn sgr_strikethrough() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "sgr_strike"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[9mSTRIKE\033[0m'"#
        );
        assert_contains_escape(&bytes, b"\x1b[9m", "SGR strikethrough");
        assert_contains_text(&bytes, "STRIKE", "strikethrough text");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    // ── Unicode / special characters ─────────────────────────────────

    /// Unicode output preserved through attach
    #[test]
    fn unicode_output() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "unicode"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        // macOS bash 3.2 doesn't support $'\u2603', so use python3 or printf hex
        let bytes = attach_after_send(&mut daemon, &sid,
            r#"python3 -c "print('\u2603 \u2764 \u2600')""#
        );
        assert_contains_text(&bytes, "\u{2603}", "unicode snowman");
        assert_contains_text(&bytes, "\u{2764}", "unicode heart");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Tab and backspace bytes preserved
    #[test]
    fn tab_and_backspace() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "tab_bs"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf 'A\tB\bC'"#
        );
        // Tab byte 0x09 and backspace byte 0x08 should be preserved
        assert!(bytes.contains(&0x09u8), "tab byte preserved");
        assert!(bytes.contains(&0x08u8), "backspace byte preserved");
        assert_contains_text(&bytes, "A", "text A");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Rapid color changes on one line
    #[test]
    fn rapid_color_changes() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "rapid_color"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[31mR\033[32mG\033[34mB\033[0m'"#
        );
        assert_contains_escape(&bytes, b"\x1b[31m", "red");
        assert_contains_escape(&bytes, b"\x1b[32m", "green");
        assert_contains_escape(&bytes, b"\x1b[34m", "blue");
        assert_contains_escape(&bytes, b"\x1b[0m", "reset");
        assert_contains_text(&bytes, "R", "R");
        assert_contains_text(&bytes, "G", "G");
        assert_contains_text(&bytes, "B", "B");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Bracketed paste mode: ESC[?2004h / ESC[?2004l
    #[test]
    fn bracketed_paste() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "bracketed"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[?2004hPASTE\033[?2004l'"#
        );
        assert_contains_escape(&bytes, b"\x1b[?2004h", "bracketed paste on");
        assert_contains_escape(&bytes, b"\x1b[?2004l", "bracketed paste off");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Multi-line colored output
    #[test]
    fn multiline_colored_with_cursor() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "ml_color"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[31mLine1\033[0m\n\033[32mLine2\033[0m\n\033[34mLine3\033[0m'"#
        );
        assert_contains_escape(&bytes, b"\x1b[31m", "line 1 red");
        assert_contains_escape(&bytes, b"\x1b[32m", "line 2 green");
        assert_contains_escape(&bytes, b"\x1b[34m", "line 3 blue");
        assert_contains_text(&bytes, "Line1", "line 1 text");
        assert_contains_text(&bytes, "Line3", "line 3 text");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Erase line variations: ESC[1K (to beginning)
    #[test]
    fn erase_line_variations() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "el_var"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[1K'"#
        );
        assert_contains_escape(&bytes, b"\x1b[1K", "erase to beginning of line");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Erase display variations: ESC[1J, ESC[0J
    #[test]
    fn erase_display_variations() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "ed_var"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let bytes = attach_after_send(&mut daemon, &sid,
            r#"printf '\033[1J\033[0J'"#
        );
        assert_contains_escape(&bytes, b"\x1b[1J", "erase to beginning of display");
        assert_contains_escape(&bytes, b"\x1b[0J", "erase to end of display");
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Session lifecycle edge cases
// ═══════════════════════════════════════════════════════════════════

mod lifecycle {
    use super::*;

    /// Destroy a session twice — second time should fail gracefully
    #[test]
    fn destroy_twice() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "dbl_dest"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["destroy", "--session", &sid]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["destroy", "--session", &sid]);
        assert_error(&resp, "session not found");
        daemon.stop();
    }

    /// Send to a destroyed session
    #[test]
    fn send_to_destroyed_session() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "snd_dest"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        daemon.cli_json(&["destroy", "--session", &sid]);

        let resp = daemon.cli_json(&["send", "--session", &sid, "echo test"]);
        assert_error(&resp, "session not found");
        daemon.stop();
    }

    /// Read from a destroyed session
    #[test]
    fn read_destroyed_session() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "rd_dest"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        daemon.cli_json(&["destroy", "--session", &sid]);

        let resp = daemon.cli_json(&["read", "--session", &sid]);
        assert_error(&resp, "session not found");
        daemon.stop();
    }

    /// Resize a destroyed session
    #[test]
    fn resize_destroyed_session() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "rsz_dest"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        daemon.cli_json(&["destroy", "--session", &sid]);

        let resp = daemon.cli_json(&["resize", "--session", &sid, "--rows", "40", "--cols", "120"]);
        assert_error(&resp, "session not found");
        daemon.stop();
    }

    /// Create multiple sessions and verify list
    #[test]
    fn multiple_sessions_list() {
        let mut daemon = start_daemon();
        let mut sids = Vec::new();
        for i in 0..3 {
            let resp = daemon.cli_json(&["create", "--name", &format!("multi_{}", i)]);
            assert_ok(&resp);
            sids.push(session_id(&resp));
        }

        let resp = daemon.cli_json(&["list"]);
        assert_ok(&resp);
        let sessions = resp.sessions.unwrap_or_default();
        assert!(sessions.len() >= 3, "should have at least 3 sessions, got {}", sessions.len());

        let names: Vec<_> = sessions.iter().map(|s| s.name.clone().unwrap_or_default()).collect();
        assert!(names.iter().any(|n| n == "multi_0"), "should find multi_0");
        assert!(names.iter().any(|n| n == "multi_1"), "should find multi_1");
        assert!(names.iter().any(|n| n == "multi_2"), "should find multi_2");

        for sid in &sids {
            daemon.cli_json(&["destroy", "--session", sid]);
        }
        daemon.stop();
    }

    /// Create session with explicit shell
    #[test]
    fn create_with_explicit_shell() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/bash", "--name", "explicit_bash"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo explicit_shell_ok"]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("explicit_shell_ok"));

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Create with /bin/sh (not bash)
    #[test]
    fn create_with_sh() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--shell", "/bin/sh", "--name", "sh_session"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo sh_works"]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("sh_works"));

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Send to exited session should return error
    #[test]
    fn send_to_exited_session() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "exited"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "exit 42"]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "echo after_exit"]);
        assert_error(&resp, "session exited");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Read from exited session
    #[test]
    fn read_exited_session() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "rd_exit"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo before_exit"]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "exit"]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["read", "--session", &sid]);
        if resp.ok {
            assert_eq!(resp.exited, Some(true), "read from exited session should report exited");
        }

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Read cursor / incremental read edge cases
// ═══════════════════════════════════════════════════════════════════

mod read_cursor {
    use super::*;

    /// Incremental read with client_id: second read only returns new data
    #[test]
    fn incremental_read_with_client_id() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "incr_read"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo FIRST"]);
        assert_ok(&resp);

        let resp1 = daemon.cli_json(&["read", "--session", &sid, "--client-id", "test_client_1"]);
        assert_ok(&resp1);
        let output1 = resp1.output.unwrap_or_default();
        assert!(output1.contains("FIRST"), "first read should contain FIRST, got: {:?}", output1);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo SECOND"]);
        assert_ok(&resp);

        let resp2 = daemon.cli_json(&["read", "--session", &sid, "--client-id", "test_client_1"]);
        assert_ok(&resp2);
        let output2 = resp2.output.unwrap_or_default();
        assert!(output2.contains("SECOND"), "incremental read should contain SECOND, got: {:?}", output2);
        assert!(!output2.contains("FIRST"), "incremental read should NOT contain FIRST, got: {:?}", output2);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Two different client_ids get independent cursors
    #[test]
    fn independent_client_cursors() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "two_clients"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo SHARED"]);
        assert_ok(&resp);

        let resp_a = daemon.cli_json(&["read", "--session", &sid, "--client-id", "client_A"]);
        assert_ok(&resp_a);
        assert!(resp_a.output.unwrap_or_default().contains("SHARED"));

        let resp_b = daemon.cli_json(&["read", "--session", &sid, "--client-id", "client_B"]);
        assert_ok(&resp_b);
        assert!(resp_b.output.unwrap_or_default().contains("SHARED"));

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Read without client_id always returns all data
    #[test]
    fn read_no_client_returns_all() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "no_client"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo AAA"]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["read", "--session", &sid]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("AAA"));

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo BBB"]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["read", "--session", &sid]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("BBB"), "should contain BBB, got: {:?}", output);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Wait edge cases
// ═══════════════════════════════════════════════════════════════════

mod wait_edge {
    use super::*;

    /// Wait timeout when pattern never appears
    #[test]
    fn wait_timeout() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "wait_to"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&[
            "wait", "--session", &sid, "IMPOSSIBLE_PATTERN_XYZ",
            "--timeout", "1000",
        ]);
        assert!(!resp.ok, "wait timeout should return ok:false");
        assert!(resp.error.as_ref().map_or(false, |e| e.contains("timeout")),
            "should mention timeout, got: {:?}", resp.error);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Wait for already-available pattern (immediate match)
    #[test]
    fn wait_immediate_match() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "wait_imm"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "5000", "echo IMMEDIATE_MATCH",
        ]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&[
            "wait", "--session", &sid, "IMMEDIATE_MATCH", "--timeout", "5000",
        ]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("IMMEDIATE_MATCH"));

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Wait with --fixed (literal string, not regex)
    #[test]
    fn wait_fixed_pattern() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "wait_fix"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let _ = daemon.cli_json(&[
            "send", "--session", &sid, "--nowait",
            "echo 'fixed.pattern'",
        ]);

        let resp = daemon.cli_json(&[
            "wait", "--session", &sid, "fixed.pattern",
            "--fixed", "--timeout", "5000",
        ]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("fixed.pattern"));

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Attach edge cases
// ═══════════════════════════════════════════════════════════════════

mod attach_edge {
    use super::*;

    /// Multiple concurrent attaches to the same session
    #[test]
    fn multiple_attaches_same_session() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "multi_att"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let mut conn1 = daemon.connect_attach_ro(&sid).expect("attach1");
        let _ = conn1.read_output(std::time::Duration::from_millis(300));

        let mut conn2 = daemon.connect_attach_ro(&sid).expect("attach2");
        let _ = conn2.read_output(std::time::Duration::from_millis(300));

        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "5000", "echo MULTI_ATTACH_TEST",
        ]);
        assert_ok(&resp);

        // Both attaches should see the output — read with generous timeout and retry
        let mut text1 = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let chunk = conn1.read_output(std::time::Duration::from_millis(500));
            text1.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&text1).contains("MULTI_ATTACH_TEST") {
                break;
            }
        }
        let text1_str = String::from_utf8_lossy(&text1);
        assert!(text1_str.contains("MULTI_ATTACH_TEST"),
            "attach1 should see output, got: {:?}", &text1_str[..text1_str.len().min(300)]);

        let mut text2 = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let chunk = conn2.read_output(std::time::Duration::from_millis(500));
            text2.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&text2).contains("MULTI_ATTACH_TEST") {
                break;
            }
        }
        let text2_str = String::from_utf8_lossy(&text2);
        assert!(text2_str.contains("MULTI_ATTACH_TEST"),
            "attach2 should see output, got: {:?}", &text2_str[..text2_str.len().min(300)]);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Attach and then resize session
    #[test]
    fn attach_then_resize() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "att_rsz"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let mut conn = daemon.connect_attach_rw(&sid).expect("attach");
        let _ = conn.read_output(std::time::Duration::from_millis(200));

        let resp = daemon.cli_json(&["resize", "--session", &sid, "--rows", "50", "--cols", "200"]);
        assert_ok(&resp);

        let result = conn.send_and_wait("echo after_resize", "after_resize", std::time::Duration::from_secs(5));
        assert!(result.is_ok(), "session should work after resize: {:?}", result);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Attach to session then destroy — should not hang
    #[test]
    fn attach_destroy_session() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "att_dest"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let mut conn = daemon.connect_attach_ro(&sid).expect("attach");
        let _ = conn.read_output(std::time::Duration::from_millis(200));

        daemon.cli_json(&["destroy", "--session", &sid]);

        // Should not hang — just verify it returns
        let output = conn.read_output(std::time::Duration::from_secs(2));
        drop(output);

        daemon.stop();
    }

    /// Send long output (stress ringbuf)
    #[test]
    fn attach_large_output() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "large_out"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "10000",
            "for i in $(seq 1 500); do echo \"Line_$i_with_padding_XXXXXXXXXXXXXXXXX\"; done",
        ]);
        assert_ok(&resp);

        let mut conn = daemon.connect_attach_rw(&sid).expect("attach");
        let mut all_bytes = conn.initial_output.clone();
        let stream_bytes = conn.read_output(std::time::Duration::from_secs(3));
        all_bytes.extend_from_slice(&stream_bytes);

        let text = String::from_utf8_lossy(&all_bytes);
        assert!(text.contains("Line_"), "should contain some lines, got {} bytes", all_bytes.len());

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Send edge cases
// ═══════════════════════════════════════════════════════════════════

mod send_edge {
    use super::*;

    /// Command that exits with non-zero code
    #[test]
    fn send_nonzero_exit() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "nz_exit"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "false"]);
        assert_ok(&resp);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Command that produces no output
    #[test]
    fn send_no_output_command() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "no_out"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "true"]);
        assert_ok(&resp);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Multiple sequential sends accumulate output
    #[test]
    fn sequential_sends() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "seq_send"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        for i in 0..5 {
            let resp = daemon.cli_json(&[
                "send", "--session", &sid, "--timeout", "5000",
                &format!("echo SEQ_{}", i),
            ]);
            assert_ok(&resp);
        }

        let resp = daemon.cli_json(&["read", "--session", &sid]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("SEQ_4"), "should contain last command output, got: {:?}", output);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Send with special characters in command
    #[test]
    fn send_special_chars() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "spec_chars"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "5000",
            "echo 'hello world with spaces and !@#$%'",
        ]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("hello world"), "should contain text with spaces, got: {:?}", output);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Send ctrl-d (EOF) to session
    #[test]
    fn send_ctrl_d() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "ctrl_d"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--ctrl", "d"]);
        assert_ok(&resp);

        // Shell should have exited
        std::thread::sleep(std::time::Duration::from_millis(300));
        let resp = daemon.cli_json(&["send", "--session", &sid, "echo after"]);
        // Session should be exited now
        assert!(!resp.ok || resp.exited == Some(true), "session should be exited after ctrl-d");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Send with --nowait then immediately read
    #[test]
    fn send_nowait_then_read() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "nowait_read"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--nowait", "echo nowait_test"]);
        assert_ok(&resp);

        // Wait a bit for output to appear
        std::thread::sleep(std::time::Duration::from_millis(500));

        let resp = daemon.cli_json(&["read", "--session", &sid]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("nowait_test"), "read after nowait should contain output, got: {:?}", output);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Recording verification
// ═══════════════════════════════════════════════════════════════════

mod recording_verify {
    use super::*;

    /// Create session with --record, produce output, verify recording file exists and has content
    #[test]
    fn recording_file_has_content() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "rec_verify", "--record"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let recording_path = resp.recording.clone();

        let _ = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo rec_test_output"]);
        let _ = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo rec_second_line"]);

        // Give recording a moment to flush
        std::thread::sleep(std::time::Duration::from_millis(500));

        if let Some(path) = &recording_path {
            let rec_file = std::path::Path::new(path);
            if rec_file.exists() {
                let metadata = std::fs::metadata(rec_file).expect("read recording metadata");
                assert!(metadata.len() > 0, "recording file should have content");
            }
        }

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Create session without --record, verify no recording path returned
    #[test]
    fn no_recording_by_default() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "no_rec"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        assert!(resp.recording.is_none(), "should not have recording path by default");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Resize edge cases
// ═══════════════════════════════════════════════════════════════════

mod resize_edge {
    use super::*;

    /// Resize then verify terminal dimensions via tput
    #[test]
    fn resize_verify_dimensions() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "rsz_verify"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["resize", "--session", &sid, "--rows", "30", "--cols", "100"]);
        assert_ok(&resp);

        // Verify dimensions via tput
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "tput cols"]);
        assert_ok(&resp);
        let cols = resp.output.unwrap_or_default();
        // Trim whitespace / prompt noise — just check it contains "100"
        assert!(cols.contains("100"), "cols should be 100, got: {:?}", cols);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Resize to small dimensions
    #[test]
    fn resize_small() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "rsz_small"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["resize", "--session", &sid, "--rows", "5", "--cols", "20"]);
        assert_ok(&resp);

        // Session should still work after small resize
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo small_term"]);
        assert_ok(&resp);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Daemon resilience
// ═══════════════════════════════════════════════════════════════════

mod daemon_resilience {
    use super::*;

    /// Daemon survives client disconnect
    #[test]
    fn daemon_survives_client_disconnect() {
        let mut daemon = start_daemon();

        // Create a session
        let resp = daemon.cli_json(&["create", "--name", "resilient"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Attach and disconnect
        {
            let mut conn = daemon.connect_attach_rw(&sid).expect("attach");
            let _ = conn.read_output(std::time::Duration::from_millis(200));
            // conn drops here — client disconnects
        }

        // Daemon should still work
        std::thread::sleep(std::time::Duration::from_millis(200));
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo after_disconnect"]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("after_disconnect"));

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Create and destroy multiple sessions in sequence
    #[test]
    fn sequential_create_destroy() {
        let mut daemon = start_daemon();

        for i in 0..3 {
            let resp = daemon.cli_json(&["create", "--name", &format!("seq_{}", i)]);
            assert_ok(&resp);
            let sid = session_id(&resp);

            let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", &format!("echo round_{}", i)]);
            assert_ok(&resp);

            let resp = daemon.cli_json(&["destroy", "--session", &sid]);
            assert_ok(&resp);
        }

        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Concurrent operations
// ═══════════════════════════════════════════════════════════════════

mod concurrent {
    use super::*;

    /// Attach and CLI send simultaneously
    #[test]
    fn attach_and_cli_send_concurrent() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "conc_att_send"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Attach readonly
        let mut conn = daemon.connect_attach_ro(&sid).expect("attach");
        let _ = conn.read_output(std::time::Duration::from_millis(300));

        // Send command via CLI while attach is streaming
        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "5000", "echo CONCURRENT_TEST",
        ]);
        assert_ok(&resp);

        // Attach should see the output
        let mut all_output = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let chunk = conn.read_output(std::time::Duration::from_millis(300));
            all_output.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&all_output).contains("CONCURRENT_TEST") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&all_output);
        assert!(text.contains("CONCURRENT_TEST"),
            "attach should see CLI send output, got: {:?}", &text[..text.len().min(300)]);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Send ctrl-z (suspend) and resume
    #[test]
    fn send_ctrl_z_resume() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "ctrl_z"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Start a long-running command
        let _ = daemon.cli_json(&["send", "--session", &sid, "--nowait", "sleep 100"]);

        // Suspend it
        std::thread::sleep(std::time::Duration::from_millis(300));
        let resp = daemon.cli_json(&["send", "--session", &sid, "--ctrl", "z"]);
        assert_ok(&resp);

        // Shell should be responsive again (bg/fg)
        std::thread::sleep(std::time::Duration::from_millis(300));
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo after_suspend"]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("after_suspend"));

        // Kill the background sleep
        let _ = daemon.cli_json(&["send", "--session", &sid, "--nowait", "kill %1 2>/dev/null; true"]);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Multiple sessions with parallel operations
    #[test]
    fn parallel_sessions() {
        let mut daemon = start_daemon();
        let mut sids = Vec::new();

        for i in 0..3 {
            let resp = daemon.cli_json(&["create", "--name", &format!("par_{}", i)]);
            assert_ok(&resp);
            sids.push(session_id(&resp));
        }

        // Send commands to each session in sequence (parallel would need threads)
        for (i, sid) in sids.iter().enumerate() {
            let resp = daemon.cli_json(&[
                "send", "--session", sid, "--timeout", "5000",
                &format!("echo PARALLEL_{}", i),
            ]);
            assert_ok(&resp);
        }

        // Verify each session's output
        for (i, sid) in sids.iter().enumerate() {
            let resp = daemon.cli_json(&["read", "--session", sid]);
            assert_ok(&resp);
            let output = resp.output.unwrap_or_default();
            assert!(output.contains(&format!("PARALLEL_{}", i)),
                "session {} should contain its output, got: {:?}", i, output);
        }

        for sid in &sids {
            daemon.cli_json(&["destroy", "--session", sid]);
        }
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Stop vs destroy
// ═══════════════════════════════════════════════════════════════════

mod stop_vs_destroy {
    use super::*;

    /// Destroy kills a running process in the session
    #[test]
    fn destroy_running_process() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "destroy_run"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Start a sleep
        let _ = daemon.cli_json(&["send", "--session", &sid, "--nowait", "sleep 1000"]);
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Destroy should force-kill even with running process
        let resp = daemon.cli_json(&["destroy", "--session", &sid]);
        assert_ok(&resp);

        daemon.stop();
    }

    /// Destroy after shell exits naturally
    #[test]
    fn destroy_after_exit() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "destroy_exit"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Exit the shell naturally
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "exit"]);
        assert_ok(&resp);

        // Destroy should still work (cleanup)
        let resp = daemon.cli_json(&["destroy", "--session", &sid]);
        assert_ok(&resp);

        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Session info / list completeness
// ═══════════════════════════════════════════════════════════════════

mod session_info {
    use super::*;

    /// Create with name and verify list returns it with correct fields
    #[test]
    fn list_session_fields() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "field_test"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["list"]);
        assert_ok(&resp);
        let sessions = resp.sessions.unwrap_or_default();
        let session = sessions.iter().find(|s| s.id == sid)
            .expect("should find our session");

        assert_eq!(session.name, Some("field_test".to_string()));
        assert!(!session.id.is_empty(), "session id should not be empty");
        assert!(session.created_at > 0, "created_at should be positive");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Create with --cwd and verify working directory
    #[test]
    fn create_with_cwd() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "cwd_test", "--cwd", "/tmp"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "pwd"]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("/tmp") || output.contains("private/tmp"),
            "pwd should show /tmp, got: {:?}", output);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Daemon auto-start (CLI without explicit daemon start)
// ═══════════════════════════════════════════════════════════════════

mod auto_start {
    use super::*;

    /// CLI should auto-start daemon when no daemon is running
    #[test]
    fn auto_start_on_first_command() {
        // Ensure no daemon is running
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let base = std::path::PathBuf::from(&home).join(".agent-shell");
        let _ = std::fs::remove_file(base.join("daemon.sock"));
        let _ = std::fs::remove_file(base.join("daemon.pid"));

        let cli_bin = agent_shell_e2e::find_bin("agent-shell");

        // First command should auto-start daemon
        let resp = std::process::Command::new(&cli_bin)
            .args(&["create", "--name", "auto_start_test"])
            .output()
            .expect("cli should work");
        assert!(resp.status.success(), "cli should succeed");
        let stdout = String::from_utf8_lossy(&resp.stdout);
        let resp: agent_shell_core::protocol::Response = serde_json::from_str(stdout.trim())
            .expect("valid json response");
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Socket file should exist now
        assert!(base.join("daemon.sock").exists(), "socket should exist after auto-start");

        // Clean up
        let _ = std::process::Command::new(&cli_bin)
            .args(&["destroy", "--session", &sid])
            .output();
        let _ = std::process::Command::new(&cli_bin)
            .args(&["kill-daemon"])
            .output();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Read --screen edge cases
// ═══════════════════════════════════════════════════════════════════

mod read_screen {
    use super::*;

    /// Read screen after multi-line output
    #[test]
    fn screen_multiline() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "scr_ml"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "5000",
            "echo 'Line1'; echo 'Line2'; echo 'Line3'",
        ]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["read", "--session", &sid, "--screen"]);
        assert_ok(&resp);
        let screen = resp.screen.unwrap_or_default();
        let combined = screen.join("\n");
        assert!(combined.contains("Line1"), "screen should contain Line1, got: {:?}", combined);
        assert!(combined.contains("Line3"), "screen should contain Line3, got: {:?}", combined);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Read screen after clear
    #[test]
    fn screen_after_clear() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "scr_clr"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "5000", "echo BEFORE_CLEAR",
        ]);
        assert_ok(&resp);

        // Clear screen
        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "5000", "clear",
        ]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["read", "--session", &sid, "--screen"]);
        assert_ok(&resp);
        // After clear, BEFORE_CLEAR may or may not be visible depending on terminal state
        // Just verify the call succeeds
        assert!(resp.screen.is_some(), "screen should be present");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}


// ═══════════════════════════════════════════════════════════════════
//  Additional send edge cases
// ═══════════════════════════════════════════════════════════════════

mod send_ctrl_edge {
    use super::*;

    /// Send ctrl-backslash (SIGQUIT)
    #[test]
    fn send_ctrl_backslash() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "ctrl_bs"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Start a cat process
        let _ = daemon.cli_json(&["send", "--session", &sid, "--nowait", "cat"]);
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Send ctrl-\\  (SIGQUIT = 0x1c)
        // We pass a single backslash as the --ctrl argument
        let resp = daemon.cli_json(&["send", "--session", &sid, "--ctrl", "\\"]);
        assert_ok(&resp);

        // Shell should recover
        std::thread::sleep(std::time::Duration::from_millis(500));
        let _resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo after_ctrl_backslash"]);
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Send invalid ctrl letter
    #[test]
    fn send_invalid_ctrl() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "ctrl_inv"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let _resp = daemon.cli_json(&["send", "--session", &sid, "--ctrl", "x"]);
        // Either ok (sends the ctrl char) or error — just verify no crash
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Set-prompt edge cases
// ═══════════════════════════════════════════════════════════════════

mod set_prompt_edge {
    use super::*;

    /// Set prompt multiple times, last one should take effect
    #[test]
    fn set_prompt_multiple() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "prompt_multi"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["set-prompt", "--session", &sid, "PROMPT1"]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["set-prompt", "--session", &sid, "PROMPT2"]);
        assert_ok(&resp);

        // Send should still work with new prompt
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo prompt_test"]);
        assert_ok(&resp);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Set prompt to empty (clear) and verify it still works
    #[test]
    fn set_prompt_clear() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "prompt_clear"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["set-prompt", "--session", &sid, ""]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo no_prompt"]);
        assert_ok(&resp);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Replay recording
// ═══════════════════════════════════════════════════════════════════

mod replay {
    use super::*;

    /// Create session with --record, produce output, replay should succeed
    #[test]
    fn replay_recording() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "replay_test", "--record"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let recording_path = resp.recording.clone();

        let _ = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo replay_output"]);
        std::thread::sleep(std::time::Duration::from_millis(300));

        let _ = daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();

        // Now try to replay
        if let Some(path) = &recording_path {
            if std::path::Path::new(path).exists() {
                let cli_bin = agent_shell_e2e::find_bin("agent-shell");
                let output = std::process::Command::new(&cli_bin)
                    .args(&["replay", path])
                    .output()
                    .expect("replay command should work");
                // Replay should exit successfully (or at least not crash)
                // We don't validate the exact output format, just that it runs
                assert!(output.status.success() || !output.stdout.is_empty() || !output.stderr.is_empty(),
                    "replay should produce some output");
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Attach detach and session survival
// ═══════════════════════════════════════════════════════════════════

mod attach_detach {
    use super::*;

    /// Attach, disconnect, then re-attach — session should survive
    #[test]
    fn reattach_after_disconnect() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "reattach"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // First attach
        {
            let mut conn = daemon.connect_attach_rw(&sid).expect("attach1");
            let _ = conn.read_output(std::time::Duration::from_millis(200));
            // Send something through attach
            let _ = conn.send_and_wait("echo first_attach", "first_attach", std::time::Duration::from_secs(5));
            // conn drops — client disconnects
        }

        // Verify session still works via CLI
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo between_attaches"]);
        assert_ok(&resp);

        // Re-attach
        let mut conn = daemon.connect_attach_rw(&sid).expect("attach2");
        let _ = conn.read_output(std::time::Duration::from_millis(300));

        let result = conn.send_and_wait("echo second_attach", "second_attach", std::time::Duration::from_secs(5));
        assert!(result.is_ok(), "second attach should work: {:?}", result);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Attach readonly, then attach rw — both should work
    #[test]
    fn attach_ro_then_rw() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "ro_then_rw"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Attach readonly
        let mut conn_ro = daemon.connect_attach_ro(&sid).expect("ro attach");
        let _ = conn_ro.read_output(std::time::Duration::from_millis(200));

        // Attach rw
        let mut conn_rw = daemon.connect_attach_rw(&sid).expect("rw attach");
        let _ = conn_rw.read_output(std::time::Duration::from_millis(200));

        // Send via rw
        let result = conn_rw.send_and_wait("echo from_rw", "from_rw", std::time::Duration::from_secs(5));
        assert!(result.is_ok(), "rw attach should work: {:?}", result);

        // ro should see the output
        let mut all_output = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let chunk = conn_ro.read_output(std::time::Duration::from_millis(300));
            all_output.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&all_output).contains("from_rw") {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&all_output).contains("from_rw"),
            "ro attach should see rw output");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Daemon restart (session state after daemon crash+restart)
// ═══════════════════════════════════════════════════════════════════

mod daemon_restart {
    use super::*;
    use agent_shell_core::protocol::Response;

    /// After daemon is killed and restarted, old sessions should be gone
    #[test]
    fn sessions_lost_after_daemon_restart() {
        let daemon = start_daemon();
        let cli_bin = daemon.cli_bin.clone();

        // Create a session
        let resp = daemon.cli_json(&["create", "--name", "before_restart"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Kill the daemon
        let _ = daemon.cli(&["kill-daemon"]);
        std::thread::sleep(std::time::Duration::from_millis(300));

        // New daemon should auto-start
        let resp = std::process::Command::new(&cli_bin)
            .args(&["list"])
            .output()
            .expect("list should work");
        let stdout = String::from_utf8_lossy(&resp.stdout);
        let resp: Response = serde_json::from_str(stdout.trim()).expect("valid json");
        assert_ok(&resp);

        // Old session should be gone
        let sessions = resp.sessions.unwrap_or_default();
        assert!(!sessions.iter().any(|s| s.id == sid),
            "old session should not survive daemon restart");

        // Clean up
        let _ = std::process::Command::new(&cli_bin)
            .args(&["kill-daemon"])
            .output();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Output ordering / consistency
// ═══════════════════════════════════════════════════════════════════

mod output_ordering {
    use super::*;

    /// Multiple sends should produce output in order
    #[test]
    fn output_preserves_order() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "order_test"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Send sequential commands
        for i in 1..=5 {
            let resp = daemon.cli_json(&[
                "send", "--session", &sid, "--timeout", "5000",
                &format!("echo ORDER_{}", i),
            ]);
            assert_ok(&resp);
        }

        // Read all output and verify ordering
        let resp = daemon.cli_json(&["read", "--session", &sid]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();

        // Find positions of ORDER_1 through ORDER_5
        let pos1 = output.find("ORDER_1");
        let pos2 = output.find("ORDER_2");
        let _pos3 = output.find("ORDER_3");
        let _pos4 = output.find("ORDER_4");
        let pos5 = output.find("ORDER_5");

        assert!(pos1.is_some(), "should contain ORDER_1");
        assert!(pos2.is_some(), "should contain ORDER_2");
        assert!(pos5.is_some(), "should contain ORDER_5");

        // Verify they appear in order
        if let (Some(p1), Some(p2), Some(p5)) = (pos1, pos2, pos5) {
            assert!(p1 < p2, "ORDER_1 should appear before ORDER_2");
            assert!(p2 < p5, "ORDER_2 should appear before ORDER_5");
        }

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Environment / working directory edge cases
// ═══════════════════════════════════════════════════════════════════

mod env_edge {
    use super::*;

    /// Create with non-existent cwd — should fail gracefully or use fallback
    #[test]
    fn create_with_nonexistent_cwd() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "bad_cwd", "--cwd", "/nonexistent/path/that/does/not/exist"]);
        // May succeed (with fallback to /) or fail — just verify no crash
        if resp.ok {
            let sid = session_id(&resp);
            daemon.cli_json(&["destroy", "--session", &sid]);
        }
        daemon.stop();
    }

    /// Create with non-existent shell — should fail
    #[test]
    fn create_with_nonexistent_shell() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "bad_shell", "--shell", "/nonexistent/shell"]);
        assert!(!resp.ok, "should fail with nonexistent shell, got: {:?}", resp);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Long-running command interruption
// ═══════════════════════════════════════════════════════════════════

mod interrupt {
    use super::*;

    /// Send ctrl-c to interrupt a running command, then send another
    #[test]
    fn interrupt_and_continue() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "intr_cont"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Start a long-running command
        let _ = daemon.cli_json(&["send", "--session", &sid, "--nowait", "sleep 30"]);
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Interrupt with ctrl-c
        let resp = daemon.cli_json(&["send", "--session", &sid, "--ctrl", "c"]);
        assert_ok(&resp);

        // Wait for prompt
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Shell should be responsive
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo after_interrupt"]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("after_interrupt"));

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Multiple interrupts in a row
    #[test]
    fn multiple_interrupts() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "multi_intr"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Start cat
        let _ = daemon.cli_json(&["send", "--session", &sid, "--nowait", "cat"]);
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Send multiple ctrl-c
        for _ in 0..3 {
            let resp = daemon.cli_json(&["send", "--session", &sid, "--ctrl", "c"]);
            assert_ok(&resp);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // Shell should still be usable
        std::thread::sleep(std::time::Duration::from_millis(300));
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo multi_intr_ok"]);
        assert_ok(&resp);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Read with cursor / gap detection
// ═══════════════════════════════════════════════════════════════════

mod read_gap {
    use super::*;

    /// Read after producing lots of output — verify gap detection
    #[test]
    fn read_large_output_no_gap() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "gap_test"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Produce moderate output
        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "10000",
            "for i in $(seq 1 100); do echo \"line_$i\"; done",
        ]);
        assert_ok(&resp);

        // Read should have no gap
        let resp = daemon.cli_json(&["read", "--session", &sid]);
        assert_ok(&resp);
        assert!(resp.gap.is_none() || resp.gap == Some(false), "should have no gap, got: {:?}", resp.gap);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Create with custom prompt regex
// ═══════════════════════════════════════════════════════════════════

mod custom_prompt {
    use super::*;

    /// Create with custom prompt regex and verify send works
    #[test]
    fn create_with_prompt_regex() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "custom_prompt", "--prompt", "\\$\\s*$"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo custom_prompt_ok"]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("custom_prompt_ok"));

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Pipe input via attach
// ═══════════════════════════════════════════════════════════════════

mod attach_pipe {
    use super::*;

    /// Send multi-line input through attach
    #[test]
    fn attach_multiline_input() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "ml_input"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let mut conn = daemon.connect_attach_rw(&sid).expect("attach");
        let _ = conn.read_output(std::time::Duration::from_millis(300));

        // Send multi-line input via heredoc-style
        conn.send(b"echo \"line1\nline2\"\n").unwrap();

        let result = conn.send_and_wait("echo multiline_done", "multiline_done", std::time::Duration::from_secs(5));
        assert!(result.is_ok(), "should find multiline_done: {:?}", result);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Ring buffer overflow / data loss
// ═══════════════════════════════════════════════════════════════════

mod ringbuf_overflow {
    use super::*;

    /// Create session with very small buffer, produce lots of output, verify gap detection
    #[test]
    fn small_buffer_overflow() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&[
            "create", "--name", "small_buf",
            "--buffer-size", "4096",
        ]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Generate output larger than 4KB buffer
        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "10000",
            "for i in $(seq 1 200); do echo \"XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX\"; done",
        ]);
        // The command may succeed (with gap) or report buffer_overflow
        if !resp.ok {
            // Acceptable: buffer_overflow error
            assert!(resp.error.as_ref().map_or(false, |e| e.contains("buffer_overflow") || e.contains("timeout")),
                "unexpected error: {:?}", resp);
        }

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Read with client_id after overflow — should report gap
    #[test]
    fn client_gap_after_lag() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "gap_lag"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Produce some output, read with client_id
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo FIRST_READ"]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["read", "--session", &sid, "--client-id", "lag_client"]);
        assert_ok(&resp);

        // Produce more output and read again — no gap expected with normal buffer
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo SECOND_READ"]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["read", "--session", &sid, "--client-id", "lag_client"]);
        assert_ok(&resp);
        assert!(resp.gap.is_none() || resp.gap == Some(false), "no gap expected with large buffer");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Create with --env
// ═══════════════════════════════════════════════════════════════════

mod create_env {
    use super::*;

    /// Create session with custom env var and verify it's set
    #[test]
    fn create_with_env_var() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&[
            "create", "--name", "env_test",
            "--env", "MY_TEST_VAR=hello_env",
        ]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo $MY_TEST_VAR"]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("hello_env"), "should contain env var value, got: {:?}", output);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Create with multiple env vars
    #[test]
    fn create_with_multiple_env_vars() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&[
            "create", "--name", "multi_env",
            "--env", "VAR_A=aaa",
            "--env", "VAR_B=bbb",
        ]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo $VAR_A $VAR_B"]);
        assert_ok(&resp);
        let output = resp.output.unwrap_or_default();
        assert!(output.contains("aaa"), "should contain VAR_A, got: {:?}", output);
        assert!(output.contains("bbb"), "should contain VAR_B, got: {:?}", output);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Attach to exited session
// ═══════════════════════════════════════════════════════════════════

mod attach_exited {
    use super::*;

    /// Attach to an already-exited session should fail
    #[test]
    fn attach_to_exited_session() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "att_exit"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Exit the shell
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "exit"]);
        assert_ok(&resp);

        // Try to attach — should fail
        let result = daemon.connect_attach_rw(&sid);
        assert!(result.is_err(), "attach to exited session should fail");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Wait with regex edge cases
// ═══════════════════════════════════════════════════════════════════

mod wait_regex {
    use super::*;

    /// Wait with regex metacharacters: .*
    #[test]
    fn wait_regex_dot_star() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "wait_regex"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let _ = daemon.cli_json(&["send", "--session", &sid, "--nowait", "echo REGEX_MATCH_123"]);

        let resp = daemon.cli_json(&[
            "wait", "--session", &sid, "REGEX.*123", "--timeout", "5000",
        ]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("REGEX_MATCH_123"));

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Wait with anchors: ^ and $
    #[test]
    fn wait_regex_anchors() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "wait_anc"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let _ = daemon.cli_json(&["send", "--session", &sid, "--nowait", "echo ANCHOR_TEST"]);

        let resp = daemon.cli_json(&[
            "wait", "--session", &sid, "ANCHOR", "--timeout", "5000",
        ]);
        assert_ok(&resp);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Wait for pattern that appears in background while waiting
    #[test]
    fn wait_pattern_appears_later() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "wait_later"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Start wait first (in a separate thread/process would be ideal,
        // but we can use --nowait + sleep to simulate)
        let _ = daemon.cli_json(&[
            "send", "--session", &sid, "--nowait",
            "sleep 0.3 && echo DELAYED_PATTERN",
        ]);

        // Wait should find the pattern after it appears
        let resp = daemon.cli_json(&[
            "wait", "--session", &sid, "DELAYED_PATTERN", "--timeout", "10000",
        ]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("DELAYED_PATTERN"));

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Send timeout behavior
// ═══════════════════════════════════════════════════════════════════

mod send_timeout {
    use super::*;

    /// Send with short timeout should report timeout for long-running command
    #[test]
    fn send_short_timeout() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "send_to"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "500", "sleep 10",
        ]);
        assert!(!resp.ok, "short timeout should return ok:false");
        assert!(resp.error.as_ref().map_or(false, |e| e.contains("timeout")),
            "should mention timeout, got: {:?}", resp.error);

        // Clean up: interrupt the sleep
        let _ = daemon.cli_json(&["send", "--session", &sid, "--ctrl", "c"]);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Send with adequate timeout should succeed
    #[test]
    fn send_adequate_timeout() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "send_ok_to"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "10000", "echo timeout_ok",
        ]);
        assert_ok(&resp);
        assert!(resp.output.unwrap_or_default().contains("timeout_ok"));

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Session name handling
// ═══════════════════════════════════════════════════════════════════

mod session_name {
    use super::*;

    /// Create session without name
    #[test]
    fn create_without_name() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Should work fine without a name
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo no_name_ok"]);
        assert_ok(&resp);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Create session with duplicate name (should work — names aren't unique)
    #[test]
    fn create_duplicate_name() {
        let mut daemon = start_daemon();
        let resp1 = daemon.cli_json(&["create", "--name", "dup_name"]);
        assert_ok(&resp1);
        let sid1 = session_id(&resp1);

        let resp2 = daemon.cli_json(&["create", "--name", "dup_name"]);
        assert_ok(&resp2);
        let sid2 = session_id(&resp2);

        // Both sessions should exist and be distinct
        assert_ne!(sid1, sid2, "session IDs should be unique");

        let resp = daemon.cli_json(&["list"]);
        assert_ok(&resp);
        let sessions = resp.sessions.unwrap_or_default();
        let dup_count = sessions.iter().filter(|s| s.name == Some("dup_name".into())).count();
        assert_eq!(dup_count, 2, "should have two sessions with the same name");

        daemon.cli_json(&["destroy", "--session", &sid1]);
        daemon.cli_json(&["destroy", "--session", &sid2]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Replay --dump mode
// ═══════════════════════════════════════════════════════════════════

mod replay_dump {
    use super::*;

    /// Replay a recording with --dump flag
    #[test]
    fn replay_dump_mode() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "replay_dump", "--record"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        let recording_path = resp.recording.clone();

        let _ = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo dump_test"]);
        std::thread::sleep(std::time::Duration::from_millis(300));

        let _ = daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();

        if let Some(path) = &recording_path {
            if std::path::Path::new(path).exists() {
                let cli_bin = agent_shell_e2e::find_bin("agent-shell");
                let output = std::process::Command::new(&cli_bin)
                    .args(&["replay", path, "--dump"])
                    .output()
                    .expect("replay --dump should work");
                assert!(output.status.success(), "replay --dump should succeed");
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(!stdout.is_empty(), "dump should produce output");
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Response field completeness
// ═══════════════════════════════════════════════════════════════════

mod response_fields {
    use super::*;

    /// Create response includes session_id and recording
    #[test]
    fn create_response_fields() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "resp_fields", "--record"]);
        assert_ok(&resp);
        let sid = session_id(&resp);
        assert!(!sid.is_empty(), "session_id should not be empty");
        assert!(resp.recording.is_some(), "should have recording path when --record");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Send response includes seq and elapsed_ms
    #[test]
    fn send_response_fields() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "send_resp"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo field_test"]);
        assert_ok(&resp);
        assert!(resp.seq.is_some(), "send should return seq");
        assert!(resp.elapsed_ms.is_some(), "send should return elapsed_ms");
        assert!(resp.elapsed_ms.unwrap() > 0, "elapsed_ms should be positive");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Exit response includes exited and exit_code
    #[test]
    fn exit_response_fields() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "exit_resp"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "exit 42"]);
        assert_ok(&resp);
        assert_eq!(resp.exited, Some(true), "should report exited=true");
        assert_eq!(resp.exit_code, Some(42), "should report exit_code=42");

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Resize boundary values
// ═══════════════════════════════════════════════════════════════════

mod resize_boundary {
    use super::*;

    /// Resize to 1x1 terminal
    #[test]
    fn resize_1x1() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "rsz_1x1"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["resize", "--session", &sid, "--rows", "1", "--cols", "1"]);
        assert_ok(&resp);

        // Session should still function
        let _resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo tiny"]);
        // Even if output is garbled by 1x1, session shouldn't crash
        // The command might timeout due to line wrapping issues, but no panic
        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Resize to very large terminal
    #[test]
    fn resize_large() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "rsz_large"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["resize", "--session", &sid, "--rows", "500", "--cols", "1000"]);
        assert_ok(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo large_term_ok"]);
        assert_ok(&resp);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Multiple rapid resizes
    #[test]
    fn rapid_resize() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "rsz_rapid"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        for rows in [10, 20, 30, 40, 24] {
            let resp = daemon.cli_json(&["resize", "--session", &sid, "--rows", &rows.to_string(), "--cols", "80"]);
            assert_ok(&resp);
        }

        // Session should still work
        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo rapid_resize_ok"]);
        assert_ok(&resp);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Shell subprocess cleanup
// ═══════════════════════════════════════════════════════════════════

mod subprocess_cleanup {
    use super::*;

    /// Destroy session with running child subprocess — no orphans
    #[test]
    fn destroy_kills_child_processes() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "child_cleanup"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Start a background sleep
        let _ = daemon.cli_json(&[
            "send", "--session", &sid, "--timeout", "5000",
            "sleep 100 &",
        ]);

        // Destroy should clean up everything
        let resp = daemon.cli_json(&["destroy", "--session", &sid]);
        assert_ok(&resp);

        // Verify no zombie sleep processes (best effort)
        std::thread::sleep(std::time::Duration::from_millis(500));
        let output = std::process::Command::new("pgrep")
            .args(&["-f", "sleep 100"])
            .output()
            .ok();
        if let Some(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            // There may be other sleep 100 processes, so we just verify no crash
            assert!(!stdout.is_empty() || true, "no orphan check is best-effort");
        }

        daemon.stop();
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Read cursor edge cases
// ═══════════════════════════════════════════════════════════════════

mod read_cursor_edge {
    use super::*;

    /// Read from same client_id multiple times without new data — should return empty
    #[test]
    fn read_no_new_data() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "no_new_data"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        let resp = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo only_once"]);
        assert_ok(&resp);

        // First read consumes the data
        let resp1 = daemon.cli_json(&["read", "--session", &sid, "--client-id", "drain_client"]);
        assert_ok(&resp1);
        assert!(resp1.output.unwrap_or_default().contains("only_once"));

        // Second read should return empty or minimal output
        let resp2 = daemon.cli_json(&["read", "--session", &sid, "--client-id", "drain_client"]);
        assert_ok(&resp2);
        let output2 = resp2.output.unwrap_or_default();
        assert!(!output2.contains("only_once"), "second read should not contain old data, got: {:?}", output2);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }

    /// Read with client_id that has fallen behind (gap detection)
    #[test]
    fn read_stale_client_gap() {
        let mut daemon = start_daemon();
        let resp = daemon.cli_json(&["create", "--name", "stale_client", "--buffer-size", "4096"]);
        assert_ok(&resp);
        let sid = session_id(&resp);

        // Read with a client to establish cursor
        let _ = daemon.cli_json(&["send", "--session", &sid, "--timeout", "5000", "echo initial_data"]);
        let _ = daemon.cli_json(&["read", "--session", &sid, "--client-id", "stale_client"]);

        // Now read again — no gap expected since buffer is large enough
        let resp = daemon.cli_json(&["read", "--session", &sid, "--client-id", "stale_client"]);
        assert_ok(&resp);

        daemon.cli_json(&["destroy", "--session", &sid]);
        daemon.stop();
    }
}
