//! Tmux command execution.

use crate::pane::PaneId;
use displaydoc::Display;
use std::io;
use std::process::{Command, Output};
use thiserror::Error;

/// Error type for tmux operations.
#[derive(Debug, Display, Error)]
pub enum TmuxError {
    /// Tmux command '{command}' failed: {stderr}
    CommandFailed { command: String, stderr: String },
    /// Tmux session '{0}' not found
    SessionNotFound(String),
    /// Tmux target '{target}' not found. Use `tmux_list_panes` to see active panes.
    WindowNotFound { target: String },
    /// Invalid tmux window info '{line}': {reason}
    InvalidWindowInfo { line: String, reason: String },
    /// IO error: {0}
    IoError(#[from] io::Error),
}

/// Result type for tmux operations.
pub type TmuxResult<T> = Result<T, TmuxError>;

const BOOTSTRAP_WINDOW_NAME: &str = "__wrix_bootstrap__";

/// Trait for executing tmux commands, allowing for mocking in tests.
pub trait CommandExecutor: Send + Sync {
    fn execute(&self, args: &[&str]) -> io::Result<Output>;
}

/// Real command executor that runs actual tmux commands.
#[derive(Default)]
pub struct RealExecutor;

impl CommandExecutor for RealExecutor {
    fn execute(&self, args: &[&str]) -> io::Result<Output> {
        Command::new("tmux").args(args).output()
    }
}

/// Manages a tmux session for the MCP server.
pub struct TmuxSession<E: CommandExecutor = RealExecutor> {
    session_name: String,
    session_created: bool,
    executor: E,
    width: u32,
    height: u32,
}

impl TmuxSession<RealExecutor> {
    /// Create a new `TmuxSession` with default executor.
    pub fn new() -> Self {
        Self::with_executor(RealExecutor)
    }
}

impl Default for TmuxSession<RealExecutor> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: CommandExecutor> TmuxSession<E> {
    /// Create a new `TmuxSession` with a custom executor.
    pub fn with_executor(executor: E) -> Self {
        let pid = std::process::id();
        Self {
            session_name: format!("debug-{pid}"),
            session_created: false,
            executor,
            width: 200,
            height: 50,
        }
    }

    /// Get the session name.
    #[cfg(test)]
    pub fn session_name(&self) -> &str {
        &self.session_name
    }

    /// Check if the session has been created.
    #[cfg(test)]
    pub const fn is_created(&self) -> bool {
        self.session_created
    }

    fn run_tmux(&self, args: &[&str]) -> TmuxResult<String> {
        let output = self.executor.execute(args)?;

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).to_string());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let command = format!("tmux {}", args.join(" "));

        if stderr.contains("session not found") || stderr.contains("no server running") {
            Err(TmuxError::SessionNotFound(self.session_name.clone()))
        } else if stderr.contains("can't find window")
            || stderr.contains("window not found")
            || stderr.contains("no such window")
        {
            Err(TmuxError::WindowNotFound {
                target: Self::target_from_args(args),
            })
        } else {
            Err(TmuxError::CommandFailed { command, stderr })
        }
    }

    fn target_from_args(args: &[&str]) -> String {
        args.iter()
            .find(|arg| arg.contains(':'))
            .map_or_else(|| "unknown".to_string(), |arg| (*arg).to_string())
    }

    fn set_window_remain_on_exit(&self, target: &str) -> TmuxResult<()> {
        match self.run_tmux(&["set-option", "-t", target, "remain-on-exit", "on"]) {
            Ok(_) | Err(TmuxError::WindowNotFound { .. }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn remove_bootstrap_window(&self) -> TmuxResult<()> {
        let target = format!("{}:{BOOTSTRAP_WINDOW_NAME}", self.session_name);
        match self.run_tmux(&["kill-window", "-t", &target]) {
            Ok(_) | Err(TmuxError::WindowNotFound { .. }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn window_info_parts(line: &str) -> TmuxResult<(&str, &str, &str)> {
        let mut parts = line.split('|');
        let Some(pane_id_raw) = parts.next() else {
            return Err(TmuxError::InvalidWindowInfo {
                line: line.to_string(),
                reason: "expected window_name|pane_pid|pane_dead".to_string(),
            });
        };
        let Some(pid_raw) = parts.next() else {
            return Err(TmuxError::InvalidWindowInfo {
                line: line.to_string(),
                reason: "expected window_name|pane_pid|pane_dead".to_string(),
            });
        };
        let Some(dead_raw) = parts.next() else {
            return Err(TmuxError::InvalidWindowInfo {
                line: line.to_string(),
                reason: "expected window_name|pane_pid|pane_dead".to_string(),
            });
        };

        Ok((pane_id_raw, pid_raw, dead_raw))
    }

    fn window_info_from_parts(
        line: &str,
        pane_id: PaneId,
        pid_raw: &str,
        dead_raw: &str,
    ) -> TmuxResult<WindowInfo> {
        let pid = pid_raw
            .parse::<u32>()
            .map_err(|error| TmuxError::InvalidWindowInfo {
                line: line.to_string(),
                reason: format!("invalid pane pid: {error}"),
            })?;

        Ok(WindowInfo {
            pane_id,
            pid: Some(pid),
            is_dead: dead_raw == "1",
        })
    }

    fn parse_window_info_line(line: &str) -> TmuxResult<WindowInfo> {
        let (pane_id_raw, pid_raw, dead_raw) = Self::window_info_parts(line)?;
        let pane_id = PaneId::parse(pane_id_raw).map_err(|error| TmuxError::InvalidWindowInfo {
            line: line.to_string(),
            reason: format!("invalid pane id: {error}"),
        })?;

        Self::window_info_from_parts(line, pane_id, pid_raw, dead_raw)
    }

    fn parse_managed_window_info_line(line: &str) -> TmuxResult<Option<WindowInfo>> {
        let (pane_id_raw, pid_raw, dead_raw) = Self::window_info_parts(line)?;
        let Ok(pane_id) = PaneId::parse(pane_id_raw) else {
            return Ok(None);
        };

        Self::window_info_from_parts(line, pane_id, pid_raw, dead_raw).map(Some)
    }

    fn keep_recent_lines(output: &str, max_lines: i32) -> String {
        let max_lines = usize::try_from(max_lines.max(1)).map_or(1, std::convert::identity);
        let mut lines: Vec<&str> = output.lines().collect();
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        let start = lines.len().saturating_sub(max_lines);
        let mut trimmed = lines[start..].join("\n");
        if output.ends_with('\n') && !trimmed.is_empty() {
            trimmed.push('\n');
        }
        trimmed
    }

    fn ensure_session(&mut self) -> TmuxResult<()> {
        if self.session_created {
            return Ok(());
        }

        let width = self.width.to_string();
        let height = self.height.to_string();
        self.run_tmux(&[
            "new-session",
            "-d",
            "-s",
            &self.session_name,
            "-n",
            BOOTSTRAP_WINDOW_NAME,
            "-x",
            &width,
            "-y",
            &height,
        ])?;

        self.run_tmux(&[
            "set-hook",
            "-t",
            &self.session_name,
            "after-new-window",
            "set-option remain-on-exit on",
        ])?;

        self.session_created = true;
        Ok(())
    }

    /// Create a new pane running the given command.
    pub fn create_pane(&mut self, command: &str, pane_id: &PaneId) -> TmuxResult<PaneId> {
        self.ensure_session()?;

        let target = format!("{}:{}", self.session_name, pane_id.as_str());
        self.run_tmux(&[
            "new-window",
            "-t",
            &self.session_name,
            "-n",
            pane_id.as_str(),
            command,
        ])?;

        self.set_window_remain_on_exit(&target)?;
        self.remove_bootstrap_window()?;

        Ok(pane_id.clone())
    }

    /// Send keystrokes to a pane.
    pub fn send_keys(&self, pane_id: &PaneId, keys: &str) -> TmuxResult<()> {
        let target = format!("{}:{}", self.session_name, pane_id.as_str());
        self.run_tmux(&["send-keys", "-t", &target, keys])?;
        Ok(())
    }

    /// Capture output from a pane.
    pub fn capture_pane(&self, pane_id: &PaneId, lines: i32) -> TmuxResult<String> {
        let target = format!("{}:{}", self.session_name, pane_id.as_str());
        let line_limit = lines.clamp(1, 1000);
        let info = self.get_window_info(pane_id)?;
        let output = if info.is_dead {
            self.run_tmux(&[
                "capture-pane",
                "-t",
                &target,
                "-p",
                "-S",
                "-1000",
                "-E",
                "-1",
            ])?
        } else {
            self.run_tmux(&["capture-pane", "-t", &target, "-p", "-S", "-1000"])?
        };
        Ok(Self::keep_recent_lines(&output, line_limit))
    }

    /// Kill a pane/window.
    pub fn kill_pane(&mut self, pane_id: &PaneId) -> TmuxResult<()> {
        let target = format!("{}:{}", self.session_name, pane_id.as_str());
        self.run_tmux(&["kill-window", "-t", &target])?;
        match self.run_tmux(&["list-windows", "-t", &self.session_name]) {
            Ok(_) => Ok(()),
            Err(TmuxError::SessionNotFound(_)) => {
                self.session_created = false;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// List all windows in the session.
    pub fn list_windows(&self) -> TmuxResult<Vec<WindowInfo>> {
        if !self.session_created {
            return Ok(Vec::new());
        }

        let format = "#{window_name}|#{pane_pid}|#{pane_dead}";
        let output = self.run_tmux(&["list-windows", "-t", &self.session_name, "-F", format])?;

        let mut windows = Vec::new();
        for line in output.lines().filter(|line| !line.is_empty()) {
            if let Some(window) = Self::parse_managed_window_info_line(line)? {
                windows.push(window);
            }
        }
        Ok(windows)
    }

    /// Get info about a specific window.
    pub fn get_window_info(&self, pane_id: &PaneId) -> TmuxResult<WindowInfo> {
        let target = format!("{}:{}", self.session_name, pane_id.as_str());
        let format = "#{window_name}|#{pane_pid}|#{pane_dead}";
        let output = self.run_tmux(&["display-message", "-p", "-t", &target, format])?;

        let line = output
            .lines()
            .next()
            .ok_or_else(|| TmuxError::WindowNotFound {
                target: target.clone(),
            })?;
        Self::parse_window_info_line(line)
    }

    /// Kill the entire session.
    pub fn kill_session(&mut self) -> TmuxResult<()> {
        if !self.session_created {
            return Ok(());
        }

        match self.run_tmux(&["kill-session", "-t", &self.session_name]) {
            Ok(_) | Err(TmuxError::SessionNotFound(_)) => {
                self.session_created = false;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

/// Information about a tmux window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// Window name, used as `pane_id`.
    pub pane_id: PaneId,
    /// Process ID running in the pane.
    pub pid: Option<u32>,
    /// Whether the pane's process has exited.
    pub is_dead: bool,
}

impl WindowInfo {
    /// Get the status as a string.
    #[cfg(test)]
    pub const fn status(&self) -> &'static str {
        if self.is_dead { "exited" } else { "running" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_id(value: &str) -> PaneId {
        PaneId::parse(value).unwrap()
    }

    #[derive(Default)]
    struct StaticMockExecutor;

    impl CommandExecutor for StaticMockExecutor {
        fn execute(&self, args: &[&str]) -> io::Result<Output> {
            let stdout = match args.first() {
                Some(&"capture-pane") => "test output line 1\ntest output line 2\n",
                Some(&"display-message") => "debug-1|12345|0\n",
                Some(&"list-windows") => "bash|999|0\ndebug-1|12345|0\ndebug-2|12346|1\n",
                _ => "",
            };
            Ok(Output {
                status: std::process::ExitStatus::default(),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        }
    }

    struct TrackingMockExecutor {
        calls: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl TrackingMockExecutor {
        fn new() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn get_calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CommandExecutor for TrackingMockExecutor {
        fn execute(&self, args: &[&str]) -> io::Result<Output> {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(std::string::ToString::to_string).collect());

            let stdout = match args.first() {
                Some(&"display-message" | &"list-windows") => "debug-1|12345|0\n",
                Some(&"capture-pane") => "captured output\n",
                _ => "",
            };

            Ok(Output {
                status: std::process::ExitStatus::default(),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        }
    }

    #[test]
    fn test_session_name_format() {
        let session = TmuxSession::with_executor(StaticMockExecutor);
        assert!(session.session_name().starts_with("debug-"));
    }

    #[test]
    fn test_session_not_created_initially() {
        let session = TmuxSession::with_executor(StaticMockExecutor);
        assert!(!session.is_created());
    }

    #[test]
    fn test_session_created_after_create_pane() {
        let mut session = TmuxSession::with_executor(StaticMockExecutor);
        session
            .create_pane("echo hello", &pane_id("debug-1"))
            .unwrap();
        assert!(session.is_created());
    }

    #[test]
    fn test_create_pane_executes_correct_commands() {
        let executor = TrackingMockExecutor::new();
        let mut session = TmuxSession::with_executor(executor);

        session
            .create_pane("cargo run", &pane_id("debug-1"))
            .unwrap();

        let calls = session.executor.get_calls();

        assert!(calls.len() >= 4);
        assert_eq!(calls[0][0], "new-session");
        assert!(calls[0].contains(&"-d".to_string()));
        assert!(calls[0].contains(&"-s".to_string()));
        assert!(calls[0].contains(&BOOTSTRAP_WINDOW_NAME.to_string()));
        assert_eq!(calls[1][0], "set-hook");
        assert!(calls[1].contains(&"after-new-window".to_string()));
        assert!(calls[1].contains(&"set-option remain-on-exit on".to_string()));
        assert_eq!(calls[2][0], "new-window");
        assert!(calls[2].contains(&"-n".to_string()));
        assert!(calls[2].contains(&"debug-1".to_string()));
        assert!(calls[2].contains(&"cargo run".to_string()));
        assert_eq!(calls[4][0], "kill-window");
        assert!(
            calls[4]
                .iter()
                .any(|arg| arg.contains(BOOTSTRAP_WINDOW_NAME))
        );
    }

    #[test]
    fn test_send_keys_executes_correct_command() {
        let executor = TrackingMockExecutor::new();
        let mut session = TmuxSession::with_executor(executor);
        let id = pane_id("debug-1");

        session.create_pane("bash", &id).unwrap();
        session.send_keys(&id, "echo hello").unwrap();

        let calls = session.executor.get_calls();
        let send_keys_call = calls.last().unwrap();

        assert_eq!(send_keys_call[0], "send-keys");
        assert!(send_keys_call.iter().any(|s| s.contains("debug-1")));
        assert!(send_keys_call.contains(&"echo hello".to_string()));
    }

    #[test]
    fn test_capture_pane_executes_correct_command() {
        let executor = TrackingMockExecutor::new();
        let mut session = TmuxSession::with_executor(executor);
        let id = pane_id("debug-1");

        session.create_pane("bash", &id).unwrap();
        let output = session.capture_pane(&id, 100).unwrap();

        assert_eq!(output, "captured output\n");

        let calls = session.executor.get_calls();
        let capture_call = calls.last().unwrap();

        assert_eq!(capture_call[0], "capture-pane");
        assert!(capture_call.contains(&"-p".to_string()));
        assert!(capture_call.contains(&"-S".to_string()));
        assert!(capture_call.contains(&"-1000".to_string()));
    }

    #[test]
    fn test_kill_pane_executes_correct_command() {
        let executor = TrackingMockExecutor::new();
        let mut session = TmuxSession::with_executor(executor);
        let id = pane_id("debug-1");

        session.create_pane("bash", &id).unwrap();
        session.kill_pane(&id).unwrap();

        let calls = session.executor.get_calls();
        let kill_call = calls
            .iter()
            .find(|call| {
                call.first() == Some(&"kill-window".to_string())
                    && call.iter().any(|arg| arg.contains("debug-1"))
            })
            .unwrap();

        assert_eq!(kill_call[0], "kill-window");
    }

    #[test]
    fn test_list_windows_parses_output() {
        let mut session = TmuxSession::with_executor(StaticMockExecutor);

        session.create_pane("bash", &pane_id("debug-1")).unwrap();

        let windows = session.list_windows().unwrap();

        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].pane_id.as_str(), "debug-1");
        assert_eq!(windows[0].pid, Some(12345));
        assert!(!windows[0].is_dead);
        assert_eq!(windows[0].status(), "running");
        assert_eq!(windows[1].pane_id.as_str(), "debug-2");
        assert_eq!(windows[1].pid, Some(12346));
        assert!(windows[1].is_dead);
        assert_eq!(windows[1].status(), "exited");
    }

    #[test]
    fn test_list_windows_empty_when_no_session() {
        let session = TmuxSession::with_executor(StaticMockExecutor);
        let windows = session.list_windows().unwrap();
        assert!(windows.is_empty());
    }

    #[test]
    fn test_kill_session_marks_not_created() {
        let mut session = TmuxSession::with_executor(StaticMockExecutor);

        session.create_pane("bash", &pane_id("debug-1")).unwrap();
        assert!(session.is_created());

        session.kill_session().unwrap();
        assert!(!session.is_created());
    }

    #[test]
    fn test_kill_session_noop_when_not_created() {
        let mut session = TmuxSession::with_executor(StaticMockExecutor);
        session.kill_session().unwrap();
    }

    #[test]
    fn test_ensure_session_only_creates_once() {
        let executor = TrackingMockExecutor::new();
        let mut session = TmuxSession::with_executor(executor);

        session.create_pane("bash", &pane_id("debug-1")).unwrap();
        session.create_pane("bash", &pane_id("debug-2")).unwrap();

        let calls = session.executor.get_calls();
        let new_session_count = calls
            .iter()
            .filter(|c| c.first() == Some(&"new-session".to_string()))
            .count();

        assert_eq!(new_session_count, 1);
    }

    #[test]
    fn test_window_info_status_running() {
        let info = WindowInfo {
            pane_id: pane_id("debug-1"),
            pid: Some(123),
            is_dead: false,
        };
        assert_eq!(info.status(), "running");
    }

    #[test]
    fn test_window_info_status_exited() {
        let info = WindowInfo {
            pane_id: pane_id("debug-1"),
            pid: Some(123),
            is_dead: true,
        };
        assert_eq!(info.status(), "exited");
    }

    #[test]
    fn test_error_display_command_failed() {
        let err = TmuxError::CommandFailed {
            command: "tmux new-session".to_string(),
            stderr: "some error".to_string(),
        };
        let display = format!("{}", err);
        assert!(display.contains("tmux new-session"));
        assert!(display.contains("some error"));
    }

    #[test]
    fn test_error_display_session_not_found() {
        let err = TmuxError::SessionNotFound("debug-123".to_string());
        let display = format!("{}", err);
        assert!(display.contains("debug-123"));
        assert!(display.contains("not found"));
    }

    #[test]
    fn test_error_display_window_not_found() {
        let err = TmuxError::WindowNotFound {
            target: "debug-123:debug-1".to_string(),
        };
        let display = format!("{}", err);
        assert!(display.contains("debug-123:debug-1"));
        assert!(display.contains("tmux_list_panes"));
    }
}
