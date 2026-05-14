use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "DaemonConfig::default_socket_path")]
    pub socket_path: String,
    #[serde(default = "DaemonConfig::default_auto_start")]
    pub auto_start: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            socket_path: Self::default_socket_path(),
            auto_start: Self::default_auto_start(),
        }
    }
}

impl DaemonConfig {
    fn default_socket_path() -> String {
        String::new() // empty = use $HOME/.agent-shell/daemon.sock
    }
    fn default_auto_start() -> bool {
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default = "SessionConfig::default_buffer_size")]
    pub default_buffer_size: usize,
    #[serde(default = "SessionConfig::default_shell")]
    pub default_shell: String,
    #[serde(default = "SessionConfig::default_rows")]
    pub default_rows: u16,
    #[serde(default = "SessionConfig::default_cols")]
    pub default_cols: u16,
    #[serde(default)]
    pub default_prompt: String,
    #[serde(default)]
    pub record_by_default: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            default_buffer_size: Self::default_buffer_size(),
            default_shell: Self::default_shell(),
            default_rows: Self::default_rows(),
            default_cols: Self::default_cols(),
            default_prompt: String::new(),
            record_by_default: false,
        }
    }
}

impl SessionConfig {
    fn default_buffer_size() -> usize {
        524288
    }
    fn default_shell() -> String {
        "/bin/bash".to_string()
    }
    fn default_rows() -> u16 {
        24
    }
    fn default_cols() -> u16 {
        80
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingConfig {
    #[serde(default = "RecordingConfig::default_dir")]
    pub dir: String,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        RecordingConfig {
            dir: Self::default_dir(),
        }
    }
}

impl RecordingConfig {
    fn default_dir() -> String {
        "~/.agent-shell/recordings".to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub recording: RecordingConfig,
}

impl Config {
    /// Load config from `$HOME/.agent-shell/config.toml`. Returns default if not found.
    pub fn load() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let path = PathBuf::from(home).join(".agent-shell/config.toml");
        Self::load_from(path)
    }

    pub fn load_from(path: PathBuf) -> Self {
        if let Ok(content) = std::fs::read_to_string(&path) {
            toml::from_str(&content).unwrap_or_default()
        } else {
            Config::default()
        }
    }

    /// Return the effective socket path (resolving empty to default).
    /// Supports AGENT_SHELL_HOME environment variable for test isolation.
    pub fn socket_path(&self) -> PathBuf {
        if self.daemon.socket_path.is_empty() {
            Self::base_dir().join("daemon.sock")
        } else {
            let p = shellexpand::tilde(&self.daemon.socket_path).to_string();
            PathBuf::from(p)
        }
    }

    /// Return the recording directory path.
    pub fn recording_dir(&self) -> PathBuf {
        let p = shellexpand::tilde(&self.recording.dir).to_string();
        PathBuf::from(p)
    }

    /// Return the base directory.
    /// Supports AGENT_SHELL_HOME environment variable for test isolation.
    pub fn base_dir() -> PathBuf {
        if let Ok(custom) = std::env::var("AGENT_SHELL_HOME") {
            PathBuf::from(custom)
        } else {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".agent-shell")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = Config::default();
        assert_eq!(config.session.default_buffer_size, 524288);
        assert_eq!(config.session.default_shell, "/bin/bash");
        assert_eq!(config.session.default_rows, 24);
        assert_eq!(config.session.default_cols, 80);
        assert!(!config.session.record_by_default);
    }

    #[test]
    fn load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[daemon]
socket_path = "/tmp/test.sock"

[session]
default_buffer_size = 1048576
default_shell = "/bin/zsh"
"#,
        )
        .unwrap();

        let config = Config::load_from(path);
        assert_eq!(config.daemon.socket_path, "/tmp/test.sock");
        assert_eq!(config.session.default_buffer_size, 1048576);
        assert_eq!(config.session.default_shell, "/bin/zsh");
    }

    #[test]
    fn load_missing_file() {
        let config = Config::load_from(PathBuf::from("/nonexistent/config.toml"));
        assert_eq!(config.session.default_buffer_size, 524288);
    }
}
