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
        let sid1 = session_id(&resp);

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
