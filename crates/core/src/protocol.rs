use serde::{Deserialize, Serialize};

/// IPC request from CLI to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    #[serde(rename = "create")]
    Create {
        name: Option<String>,
        /// argv[0]: the executable to launch. Falls back to `default_program`
        /// in config when absent. Ignored when `args` is present.
        #[serde(alias = "shell")]
        program: Option<String>,
        /// Full argument vector. argv[0] is the executable, the rest are its
        /// arguments. When present, takes priority over `program`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Vec<String>>,
        cwd: Option<String>,
        env: Option<std::collections::HashMap<String, String>>,
        prompt: Option<String>,
        rows: Option<u16>,
        cols: Option<u16>,
        buffer_size: Option<usize>,
        record: Option<bool>,

    },

    #[serde(rename = "destroy")]
    Destroy { session_id: String },

    #[serde(rename = "send")]
    Send {
        session_id: String,
        text: String,
        ctrl: Option<String>,      // "c", "d", "z"
        nowait: Option<bool>,
        timeout_ms: Option<u64>,
        /// Output idle timeout in milliseconds. When no new output arrives for
        /// this duration, the command is considered complete. Used as the
        /// stabilization threshold regardless of fg_pgid state.
        /// Default: 150ms when fg_at_shell, 500ms when in a subprocess.
        idle_timeout_ms: Option<u64>,
        client_id: Option<String>,
    },

    #[serde(rename = "read")]
    Read {
        session_id: String,
        client_id: Option<String>,
        screen: Option<bool>,
    },

    #[serde(rename = "wait")]
    Wait {
        session_id: String,
        pattern: String,
        fixed: Option<bool>,
        timeout_ms: Option<u64>,
        client_id: Option<String>,
    },

    #[serde(rename = "set_prompt")]
    SetPrompt {
        session_id: String,
        prompt: Option<String>,
    },

    #[serde(rename = "list")]
    List,

    #[serde(rename = "attach")]
    Attach {
        session_id: String,
        writable: Option<bool>,
    },

    #[serde(rename = "resize")]
    Resize {
        session_id: String,
        rows: u16,
        cols: u16,
    },

    #[serde(rename = "mouse")]
    Mouse {
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
    },

    #[serde(rename = "stop")]
    Stop,
}

/// IPC response from daemon to CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exited: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_detected: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<SessionInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<(usize, usize)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording: Option<String>,
}

impl Response {
    pub fn ok() -> Self {
        Response {
            ok: true,
            seq: None,
            output: None,
            elapsed_ms: None,
            exited: None,
            exit_code: None,
            error: None,
            session_id: None,
            prompt_detected: None,
            sessions: None,
            screen: None,
            cursor: None,
            gap: None,
            lost_bytes: None,
            recording: None,
        }
    }

    pub fn err(error: impl Into<String>) -> Self {
        Response {
            ok: false,
            error: Some(error.into()),
            seq: None,
            output: None,
            elapsed_ms: None,
            exited: None,
            exit_code: None,
            session_id: None,
            prompt_detected: None,
            sessions: None,
            screen: None,
            cursor: None,
            gap: None,
            lost_bytes: None,
            recording: None,
        }
    }
}

/// Session metadata for `list` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub exited: bool,
    pub exit_code: Option<i32>,
    pub pid: u32,
    pub created_at: u64,
    pub buffer_size: usize,
    pub recording: bool,
    /// The program (shell) running in this session, e.g. "/bin/zsh".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Working directory at session creation time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_create() {
        let req = Request::Create {
            name: Some("test".into()),
            program: None,
            args: None,
            cwd: Some("/tmp".into()),
            env: None,
            prompt: Some("^\\$ $".into()),
            rows: None,
            cols: None,
            buffer_size: None,
            record: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let de: Request = serde_json::from_str(&json).unwrap();
        assert!(matches!(de, Request::Create { .. }));
    }

    #[test]
    fn roundtrip_response() {
        let resp = Response {
            ok: true,
            seq: Some(1),
            output: Some("hello\n".into()),
            elapsed_ms: Some(150),
            ..Response::ok()
        };
        let json = serde_json::to_string(&resp).unwrap();
        let de: Response = serde_json::from_str(&json).unwrap();
        assert!(de.ok);
        assert_eq!(de.seq, Some(1));
        assert_eq!(de.output, Some("hello\n".into()));
    }

    #[test]
    fn roundtrip_error_response() {
        let resp = Response::err("timeout");
        let json = serde_json::to_string(&resp).unwrap();
        let de: Response = serde_json::from_str(&json).unwrap();
        assert!(!de.ok);
        assert_eq!(de.error, Some("timeout".into()));
    }

    #[test]
    fn roundtrip_mouse() {
        let req = Request::Mouse {
            session_id: "abc".into(),
            action: "click".into(),
            x: 10,
            y: 5,
            button: Some("left".into()),
            direction: None,
            count: Some(2),
            to_x: None,
            to_y: None,
            steps: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let de: Request = serde_json::from_str(&json).unwrap();
        match de {
            Request::Mouse { session_id, action, x, y, button, count, .. } => {
                assert_eq!(session_id, "abc");
                assert_eq!(action, "click");
                assert_eq!(x, 10);
                assert_eq!(y, 5);
                assert_eq!(button, Some("left".into()));
                assert_eq!(count, Some(2));
            }
            _ => panic!("expected Mouse"),
        }
    }

    #[test]
    fn roundtrip_session_info() {
        let info = SessionInfo {
            id: "abc".into(),
            name: Some("ssh1".into()),
            prompt: None,
            exited: false,
            exit_code: None,
            pid: 1234,
            created_at: 1715600000,
            buffer_size: 524288,
            recording: false,
            program: Some("/bin/zsh".into()),
            cwd: Some("/home/user".into()),
        };
        let json = serde_json::to_string(&info).unwrap();
        let de: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(de.id, "abc");
    }
}
