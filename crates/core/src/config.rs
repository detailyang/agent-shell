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
    /// Default program to launch when `create` is called without an explicit
    /// program / argv. Typically a shell such as `/bin/bash` or `/bin/zsh`,
    /// but can be any executable (e.g. `/usr/bin/python3`, `vim`, …).
    #[serde(default = "SessionConfig::default_program", alias = "default_shell")]
    pub default_program: String,
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
            default_program: Self::default_program(),
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
    fn default_program() -> String {
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
    /// Delete recording files older than this many days. Set to 0 or omit to
    /// disable automatic cleanup. Default: 30 days.
    #[serde(default = "RecordingConfig::default_retention_days")]
    pub retention_days: u64,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        RecordingConfig {
            dir: Self::default_dir(),
            retention_days: Self::default_retention_days(),
        }
    }
}

impl RecordingConfig {
    fn default_dir() -> String {
        "~/.agent-shell/recordings".to_string()
    }
    fn default_retention_days() -> u64 {
        30
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
    /// Respects AGENT_SHELL_HOME for test isolation. If the user has explicitly
    /// configured `recording.dir` in config.toml (non-default), that takes priority.
    pub fn recording_dir(&self) -> PathBuf {
        // If user explicitly set a custom recording dir, respect it.
        if self.recording.dir != RecordingConfig::default_dir() {
            let p = shellexpand::tilde(&self.recording.dir).to_string();
            return PathBuf::from(p);
        }
        // Otherwise, derive from base_dir (which respects AGENT_SHELL_HOME)
        Self::base_dir().join("recordings")
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
        assert_eq!(config.session.default_program, "/bin/bash");
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
        assert_eq!(config.session.default_program, "/bin/zsh");
    }

    #[test]
    fn load_missing_file() {
        let config = Config::load_from(PathBuf::from("/nonexistent/config.toml"));
        assert_eq!(config.session.default_buffer_size, 524288);
    }

    #[test]
    fn recording_retention_days_default_and_override() {
        // Default must be 30.
        let cfg = Config::default();
        assert_eq!(cfg.recording.retention_days, 30);

        // User can set retention_days = 0 to disable cleanup.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[recording]
retention_days = 0
"#,
        )
        .unwrap();
        let cfg = Config::load_from(path);
        assert_eq!(cfg.recording.retention_days, 0);

        // User can override to a custom number of days.
        let dir2 = tempfile::tempdir().unwrap();
        let path2 = dir2.path().join("config.toml");
        std::fs::write(
            &path2,
            r#"
[recording]
retention_days = 90
"#,
        )
        .unwrap();
        let cfg2 = Config::load_from(path2);
        assert_eq!(cfg2.recording.retention_days, 90);
    }

    /// Test recording_dir behavior with AGENT_SHELL_HOME.
    /// Combined into one test to avoid env var races in parallel execution.
    #[test]
    fn recording_dir_env_behavior() {
        let prev = std::env::var("AGENT_SHELL_HOME").ok();

        // Part 1: AGENT_SHELL_HOME set → recording_dir uses it
        std::env::set_var("AGENT_SHELL_HOME", "/tmp/test_home_rec");
        let config = Config::default();
        let dir = config.recording_dir();
        assert_eq!(dir, PathBuf::from("/tmp/test_home_rec/recordings"));

        // Part 2: explicit recording.dir overrides AGENT_SHELL_HOME
        let mut config2 = Config::default();
        config2.recording.dir = "/custom/recordings".to_string();
        let dir2 = config2.recording_dir();
        assert_eq!(dir2, PathBuf::from("/custom/recordings"));

        // Part 3: no AGENT_SHELL_HOME → falls back to ~/.agent-shell/recordings
        std::env::remove_var("AGENT_SHELL_HOME");
        let config3 = Config::default();
        let dir3 = config3.recording_dir();
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        assert_eq!(dir3, PathBuf::from(home).join(".agent-shell/recordings"));

        // Restore
        match prev {
            Some(v) => std::env::set_var("AGENT_SHELL_HOME", v),
            None => {} // already removed above
        }
    }
}
