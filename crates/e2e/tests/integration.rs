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

    /// Helper: create a session, send a printf command, then attach and read raw bytes.
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

    /// Verify that a specific byte sequence appears in the attach output.
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

    /// Verify that a specific text string appears in the attach output.
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

    // ── SGR (Select Graphic Rendition) color sequences ────────────────

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
}
