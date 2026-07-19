//! Optional audit logging
//!
//! This module provides audit logging for all MCP tool operations.
//! Enabled via environment variables:
//!
//! - `TMUX_DEBUG_AUDIT`: Path to the audit log file (JSON Lines format)
//! - `TMUX_DEBUG_AUDIT_FULL`: Directory for full capture files
//!
//! Log entries include timestamp, tool name, parameters, and output size.
//! Output content is logged by byte count only unless full capture is enabled.

use crate::pane::PaneId;
use serde::Serialize;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Environment variable for audit log path
pub const AUDIT_LOG_ENV: &str = "TMUX_DEBUG_AUDIT";

/// Environment variable for full capture directory
pub const AUDIT_FULL_ENV: &str = "TMUX_DEBUG_AUDIT_FULL";

/// Tool names written to the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    CreatePane,
    SendKeys,
    CapturePane,
    KillPane,
    ListPanes,
}

/// A single audit log entry
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    /// ISO 8601 timestamp
    pub ts: String,
    /// Tool name without `tmux_` prefix
    pub tool: Tool,
    /// Pane ID (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    /// Command that was executed for `create_pane`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Pane name for `create_pane`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Keys sent for `send_keys`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<String>,
    /// Number of lines captured for `capture_pane`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<i32>,
    /// Output byte count for `capture_pane`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<usize>,
}

impl AuditEntry {
    /// Create an entry for `create_pane`
    pub fn create_pane(pane_id: &PaneId, command: &str, name: Option<&str>) -> Self {
        Self {
            ts: Self::timestamp(),
            tool: Tool::CreatePane,
            pane_id: Some(pane_id.clone()),
            command: Some(command.to_string()),
            name: name.map(std::string::ToString::to_string),
            keys: None,
            lines: None,
            output_bytes: None,
        }
    }

    /// Create an entry for `send_keys`
    pub fn send_keys(pane_id: &PaneId, keys: &str) -> Self {
        Self {
            ts: Self::timestamp(),
            tool: Tool::SendKeys,
            pane_id: Some(pane_id.clone()),
            command: None,
            name: None,
            keys: Some(keys.to_string()),
            lines: None,
            output_bytes: None,
        }
    }

    /// Create an entry for `capture_pane`
    pub fn capture_pane(pane_id: &PaneId, lines: i32, output_bytes: usize) -> Self {
        Self {
            ts: Self::timestamp(),
            tool: Tool::CapturePane,
            pane_id: Some(pane_id.clone()),
            command: None,
            name: None,
            keys: None,
            lines: Some(lines),
            output_bytes: Some(output_bytes),
        }
    }

    /// Create an entry for `kill_pane`
    pub fn kill_pane(pane_id: &PaneId) -> Self {
        Self {
            ts: Self::timestamp(),
            tool: Tool::KillPane,
            pane_id: Some(pane_id.clone()),
            command: None,
            name: None,
            keys: None,
            lines: None,
            output_bytes: None,
        }
    }

    /// Create an entry for `list_panes`
    pub fn list_panes() -> Self {
        Self {
            ts: Self::timestamp(),
            tool: Tool::ListPanes,
            pane_id: None,
            command: None,
            name: None,
            keys: None,
            lines: None,
            output_bytes: None,
        }
    }

    /// Get current timestamp in ISO 8601 format
    fn timestamp() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};

        let Ok(duration) = SystemTime::now().duration_since(UNIX_EPOCH) else {
            return "1970-01-01T00:00:00Z".to_string();
        };

        let (year, month, day, hour, min, sec) = Self::timestamp_parts(duration.as_secs());

        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            year, month, day, hour, min, sec
        )
    }

    /// Convert Unix timestamp to date/time parts
    fn timestamp_parts(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
        let day_secs = secs % 86_400;
        let hour = u32::try_from(day_secs / 3_600).map_or(0, std::convert::identity);
        let min = u32::try_from((day_secs % 3_600) / 60).map_or(0, std::convert::identity);
        let sec = u32::try_from(day_secs % 60).map_or(0, std::convert::identity);
        let mut year = 1970;
        let mut remaining_days = secs / 86_400;

        loop {
            let days_in_year = if Self::is_leap_year(year) { 366 } else { 365 };
            if remaining_days < days_in_year {
                break;
            }
            remaining_days -= days_in_year;
            year = year.saturating_add(1);
        }

        let leap = Self::is_leap_year(year);
        let days_in_months: [u64; 12] = if leap {
            [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        } else {
            [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        };

        let mut month = 1_u32;
        for days_in_month in days_in_months {
            if remaining_days < days_in_month {
                break;
            }
            remaining_days -= days_in_month;
            month += 1;
        }

        let day = u32::try_from(remaining_days + 1).map_or(1, std::convert::identity);

        (year, month, day, hour, min, sec)
    }

    /// Check if a year is a leap year
    const fn is_leap_year(year: i32) -> bool {
        (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
    }

    /// Serialize entry to JSON
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Audit logger that writes to a file and optionally saves full captures
pub struct AuditLogger {
    /// Path to the audit log file
    log_path: PathBuf,
    /// Directory for full captures (if enabled)
    full_capture_dir: Option<PathBuf>,
    /// Counter for capture file naming
    capture_counter: AtomicU64,
}

impl AuditLogger {
    /// Create a new `AuditLogger` from environment variables
    ///
    /// Returns `None` if `TMUX_DEBUG_AUDIT` is not set.
    pub fn from_env() -> Option<Self> {
        let log_path = env::var_os(AUDIT_LOG_ENV)?;
        let full_capture_dir = env::var_os(AUDIT_FULL_ENV).map(PathBuf::from);

        Some(Self {
            log_path: PathBuf::from(log_path),
            full_capture_dir,
            capture_counter: AtomicU64::new(1),
        })
    }

    /// Create a new `AuditLogger` with explicit paths
    #[cfg(test)]
    pub fn new(log_path: impl Into<PathBuf>, full_capture_dir: Option<PathBuf>) -> Self {
        Self {
            log_path: log_path.into(),
            full_capture_dir,
            capture_counter: AtomicU64::new(1),
        }
    }

    /// Check if full capture is enabled
    #[cfg(test)]
    pub const fn has_full_capture(&self) -> bool {
        self.full_capture_dir.is_some()
    }

    /// Get the log path
    #[cfg(test)]
    pub fn log_path(&self) -> &std::path::Path {
        &self.log_path
    }

    /// Get the full capture directory
    #[cfg(test)]
    pub fn full_capture_dir(&self) -> Option<&std::path::Path> {
        self.full_capture_dir.as_deref()
    }

    /// Log an audit entry
    ///
    /// Appends the entry as a JSON line to the audit log file.
    pub fn log(&self, entry: &AuditEntry) -> io::Result<()> {
        let json = entry
            .to_json()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Ensure parent directory exists
        if let Some(parent) = self.log_path.parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent)?;
        }

        // Open file in append mode
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        writeln!(file, "{}", json)?;
        Ok(())
    }

    /// Save full capture content to a file
    ///
    /// Returns the filename if saved, or `None` if full capture is not enabled.
    pub fn save_full_capture(&self, pane_id: &PaneId, content: &str) -> io::Result<Option<String>> {
        let Some(capture_dir) = &self.full_capture_dir else {
            return Ok(None);
        };

        // Ensure capture directory exists
        if !capture_dir.exists() {
            fs::create_dir_all(capture_dir)?;
        }

        // Generate unique filename
        let counter = self.capture_counter.fetch_add(1, Ordering::SeqCst);
        let filename = format!("{}-capture-{:03}.txt", pane_id, counter);
        let path = capture_dir.join(&filename);

        // Write content
        let mut file = File::create(&path)?;
        file.write_all(content.as_bytes())?;

        Ok(Some(filename))
    }
}

/// Optional audit logger wrapper for easy use throughout the codebase
pub struct MaybeAuditLogger(Option<AuditLogger>);

impl MaybeAuditLogger {
    /// Create from environment variables
    pub fn from_env() -> Self {
        Self(AuditLogger::from_env())
    }

    /// Create with an explicit logger
    #[cfg(test)]
    pub const fn new(logger: Option<AuditLogger>) -> Self {
        Self(logger)
    }

    /// Create a disabled logger
    #[cfg(test)]
    pub const fn disabled() -> Self {
        Self(None)
    }

    /// Check if logging is enabled
    #[cfg(test)]
    pub const fn is_enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Log an audit entry (no-op if disabled)
    pub fn log(&self, entry: &AuditEntry) -> io::Result<()> {
        self.0.as_ref().map_or(Ok(()), |logger| logger.log(entry))
    }

    /// Log `create_pane` (no-op if disabled)
    pub fn log_create_pane(
        &self,
        pane_id: &PaneId,
        command: &str,
        name: Option<&str>,
    ) -> io::Result<()> {
        self.log(&AuditEntry::create_pane(pane_id, command, name))
    }

    /// Log `send_keys` (no-op if disabled)
    pub fn log_send_keys(&self, pane_id: &PaneId, keys: &str) -> io::Result<()> {
        self.log(&AuditEntry::send_keys(pane_id, keys))
    }

    /// Log `capture_pane` (no-op if disabled)
    pub fn log_capture_pane(
        &self,
        pane_id: &PaneId,
        lines: i32,
        output_bytes: usize,
    ) -> io::Result<()> {
        self.log(&AuditEntry::capture_pane(pane_id, lines, output_bytes))
    }

    /// Log `kill_pane` (no-op if disabled)
    pub fn log_kill_pane(&self, pane_id: &PaneId) -> io::Result<()> {
        self.log(&AuditEntry::kill_pane(pane_id))
    }

    /// Log `list_panes` (no-op if disabled)
    pub fn log_list_panes(&self) -> io::Result<()> {
        self.log(&AuditEntry::list_panes())
    }

    /// Save full capture if enabled
    pub fn save_full_capture(&self, pane_id: &PaneId, content: &str) -> io::Result<Option<String>> {
        self.0.as_ref().map_or(Ok(None), |logger| {
            logger.save_full_capture(pane_id, content)
        })
    }
}

impl Default for MaybeAuditLogger {
    fn default() -> Self {
        Self::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn pane_id(value: &str) -> PaneId {
        PaneId::parse(value).unwrap()
    }

    fn pane_id_text(entry: &AuditEntry) -> Option<&str> {
        entry.pane_id.as_ref().map(PaneId::as_str)
    }

    // --- AuditEntry Tests ---

    #[test]
    fn test_audit_entry_create_pane() {
        let entry = AuditEntry::create_pane(&pane_id("debug-1"), "cargo run", Some("server"));

        assert_eq!(entry.tool, Tool::CreatePane);
        assert_eq!(pane_id_text(&entry), Some("debug-1"));
        assert_eq!(entry.command, Some("cargo run".to_string()));
        assert_eq!(entry.name, Some("server".to_string()));
        assert!(entry.keys.is_none());
        assert!(entry.lines.is_none());
        assert!(entry.output_bytes.is_none());
    }

    #[test]
    fn test_audit_entry_create_pane_no_name() {
        let entry = AuditEntry::create_pane(&pane_id("debug-1"), "cargo run", None);

        assert_eq!(entry.tool, Tool::CreatePane);
        assert!(entry.name.is_none());
    }

    #[test]
    fn test_audit_entry_send_keys() {
        let entry = AuditEntry::send_keys(&pane_id("debug-1"), "curl -X POST localhost:3000");

        assert_eq!(entry.tool, Tool::SendKeys);
        assert_eq!(pane_id_text(&entry), Some("debug-1"));
        assert_eq!(entry.keys, Some("curl -X POST localhost:3000".to_string()));
        assert!(entry.command.is_none());
    }

    #[test]
    fn test_audit_entry_capture_pane() {
        let entry = AuditEntry::capture_pane(&pane_id("debug-1"), 200, 4523);

        assert_eq!(entry.tool, Tool::CapturePane);
        assert_eq!(pane_id_text(&entry), Some("debug-1"));
        assert_eq!(entry.lines, Some(200));
        assert_eq!(entry.output_bytes, Some(4523));
    }

    #[test]
    fn test_audit_entry_kill_pane() {
        let entry = AuditEntry::kill_pane(&pane_id("debug-1"));

        assert_eq!(entry.tool, Tool::KillPane);
        assert_eq!(pane_id_text(&entry), Some("debug-1"));
    }

    #[test]
    fn test_audit_entry_list_panes() {
        let entry = AuditEntry::list_panes();

        assert_eq!(entry.tool, Tool::ListPanes);
        assert!(entry.pane_id.is_none());
    }

    // --- JSON Serialization Tests ---

    #[test]
    fn test_audit_entry_json_create_pane() {
        let entry = AuditEntry::create_pane(&pane_id("debug-1"), "cargo run", Some("server"));
        let json = entry.to_json().unwrap();

        assert!(json.contains(r#""tool":"create_pane""#));
        assert!(json.contains(r#""pane_id":"debug-1""#));
        assert!(json.contains(r#""command":"cargo run""#));
        assert!(json.contains(r#""name":"server""#));
        // Should NOT contain null fields
        assert!(!json.contains("keys"));
        assert!(!json.contains("lines"));
        assert!(!json.contains("output_bytes"));
    }

    #[test]
    fn test_audit_entry_json_capture_pane() {
        let entry = AuditEntry::capture_pane(&pane_id("debug-1"), 100, 5000);
        let json = entry.to_json().unwrap();

        assert!(json.contains(r#""tool":"capture_pane""#));
        assert!(json.contains(r#""pane_id":"debug-1""#));
        assert!(json.contains(r#""lines":100"#));
        assert!(json.contains(r#""output_bytes":5000"#));
        // Should NOT contain irrelevant fields
        assert!(!json.contains("command"));
        assert!(!json.contains("keys"));
    }

    #[test]
    fn test_audit_entry_json_list_panes() {
        let entry = AuditEntry::list_panes();
        let json = entry.to_json().unwrap();

        assert!(json.contains(r#""tool":"list_panes""#));
        // Minimal entry - only ts and tool
        assert!(!json.contains("pane_id"));
    }

    #[test]
    fn test_audit_entry_timestamp_format() {
        let entry = AuditEntry::list_panes();

        // Timestamp should be ISO 8601 format: YYYY-MM-DDTHH:MM:SSZ
        assert!(
            entry.ts.len() == 20,
            "Timestamp length should be 20: {}",
            entry.ts
        );
        assert!(
            entry.ts.ends_with('Z'),
            "Timestamp should end with Z: {}",
            entry.ts
        );
        assert!(
            entry.ts.contains('T'),
            "Timestamp should contain T: {}",
            entry.ts
        );
        assert_eq!(entry.ts.chars().filter(|c| *c == '-').count(), 2);
        assert_eq!(entry.ts.chars().filter(|c| *c == ':').count(), 2);
    }

    // --- Timestamp Calculation Tests ---

    #[test]
    fn test_timestamp_parts_unix_epoch() {
        let (year, month, day, hour, min, sec) = AuditEntry::timestamp_parts(0);
        assert_eq!((year, month, day, hour, min, sec), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn test_timestamp_parts_known_date() {
        // 2026-01-30T10:15:32Z = 1769681732 seconds since epoch
        // Let's verify a simpler known date first: 2000-01-01T00:00:00Z
        // = 946684800 seconds
        let (year, month, day, hour, min, sec) = AuditEntry::timestamp_parts(946_684_800);
        assert_eq!((year, month, day, hour, min, sec), (2000, 1, 1, 0, 0, 0));
    }

    #[test]
    fn test_timestamp_parts_leap_year() {
        // 2000-02-29T12:00:00Z (leap year)
        // Jan 2000: 31 days = 2678400 seconds
        // Feb 1-28: 28 days = 2419200 seconds
        // Feb 29: +12 hours = 43200 seconds
        // Total from Jan 1 2000: 31 + 28 days + 12 hours = 59 days + 12 hours
        // = 5097600 seconds from 2000-01-01
        // From epoch: 946684800 + 5097600 = 951782400
        let (year, month, day, hour, min, sec) = AuditEntry::timestamp_parts(951_782_400);
        assert_eq!((year, month, day, hour, min, sec), (2000, 2, 29, 0, 0, 0));
    }

    #[test]
    fn test_is_leap_year() {
        assert!(AuditEntry::is_leap_year(2000)); // Divisible by 400
        assert!(!AuditEntry::is_leap_year(1900)); // Divisible by 100 but not 400
        assert!(AuditEntry::is_leap_year(2004)); // Divisible by 4
        assert!(!AuditEntry::is_leap_year(2001)); // Not divisible by 4
    }

    // --- AuditLogger File Tests ---

    #[test]
    fn test_audit_logger_new() {
        let logger = AuditLogger::new("/tmp/test.log", None);

        assert_eq!(logger.log_path(), Path::new("/tmp/test.log"));
        assert!(logger.full_capture_dir().is_none());
        assert!(!logger.has_full_capture());
    }

    #[test]
    fn test_audit_logger_with_full_capture() {
        let logger = AuditLogger::new("/tmp/test.log", Some(PathBuf::from("/tmp/captures")));

        assert!(logger.has_full_capture());
        assert_eq!(logger.full_capture_dir(), Some(Path::new("/tmp/captures")));
    }

    #[test]
    fn test_audit_logger_log_writes_to_file() {
        let temp_dir = TempDir::new().unwrap();
        let log_path = temp_dir.path().join("audit.log");

        let logger = AuditLogger::new(&log_path, None);

        let entry = AuditEntry::create_pane(&pane_id("debug-1"), "cargo run", Some("server"));
        logger.log(&entry).unwrap();

        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains(r#""tool":"create_pane""#));
        assert!(content.contains(r#""pane_id":"debug-1""#));
        assert!(content.ends_with('\n'));
    }

    #[test]
    fn test_audit_logger_log_appends() {
        let temp_dir = TempDir::new().unwrap();
        let log_path = temp_dir.path().join("audit.log");

        let logger = AuditLogger::new(&log_path, None);

        // Log multiple entries
        logger
            .log(&AuditEntry::create_pane(
                &pane_id("debug-1"),
                "cargo run",
                None,
            ))
            .unwrap();
        logger
            .log(&AuditEntry::send_keys(&pane_id("debug-1"), "test"))
            .unwrap();
        logger
            .log(&AuditEntry::kill_pane(&pane_id("debug-1")))
            .unwrap();

        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();

        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("create_pane"));
        assert!(lines[1].contains("send_keys"));
        assert!(lines[2].contains("kill_pane"));
    }

    #[test]
    fn test_audit_logger_creates_parent_dirs() {
        let temp_dir = TempDir::new().unwrap();
        let log_path = temp_dir.path().join("subdir/nested/audit.log");

        let logger = AuditLogger::new(&log_path, None);
        logger.log(&AuditEntry::list_panes()).unwrap();

        assert!(log_path.exists());
    }

    #[test]
    fn test_audit_logger_json_lines_format() {
        let temp_dir = TempDir::new().unwrap();
        let log_path = temp_dir.path().join("audit.log");

        let logger = AuditLogger::new(&log_path, None);
        logger
            .log(&AuditEntry::create_pane(
                &pane_id("debug-1"),
                "cargo run",
                None,
            ))
            .unwrap();
        logger
            .log(&AuditEntry::capture_pane(&pane_id("debug-1"), 100, 5000))
            .unwrap();

        let content = fs::read_to_string(&log_path).unwrap();

        // Each line should be valid JSON
        for line in content.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|_| panic!("Line should be valid JSON: {line}"));
            assert!(parsed.is_object());
        }
    }

    // --- Full Capture Tests ---

    #[test]
    fn test_audit_logger_save_full_capture_disabled() {
        let logger = AuditLogger::new("/tmp/test.log", None);

        let result = logger
            .save_full_capture(&pane_id("debug-1"), "some content")
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_audit_logger_save_full_capture_enabled() {
        let temp_dir = TempDir::new().unwrap();
        let log_path = temp_dir.path().join("audit.log");
        let capture_dir = temp_dir.path().join("captures");

        let logger = AuditLogger::new(&log_path, Some(capture_dir.clone()));

        let filename = logger
            .save_full_capture(&pane_id("debug-1"), "captured content")
            .unwrap();

        assert!(filename.is_some());
        let filename = filename.unwrap();
        assert!(filename.starts_with("debug-1-capture-"));
        assert!(
            std::path::Path::new(&filename)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
        );

        // Verify file was created with correct content
        let file_path = capture_dir.join(&filename);
        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "captured content");
    }

    #[test]
    fn test_audit_logger_full_capture_sequential_naming() {
        let temp_dir = TempDir::new().unwrap();
        let log_path = temp_dir.path().join("audit.log");
        let capture_dir = temp_dir.path().join("captures");

        let logger = AuditLogger::new(&log_path, Some(capture_dir));

        let f1 = logger
            .save_full_capture(&pane_id("debug-1"), "content 1")
            .unwrap()
            .unwrap();
        let f2 = logger
            .save_full_capture(&pane_id("debug-1"), "content 2")
            .unwrap()
            .unwrap();
        let f3 = logger
            .save_full_capture(&pane_id("debug-2"), "content 3")
            .unwrap()
            .unwrap();

        // Filenames should have sequential numbers
        assert_eq!(f1, "debug-1-capture-001.txt");
        assert_eq!(f2, "debug-1-capture-002.txt");
        assert_eq!(f3, "debug-2-capture-003.txt");
    }

    #[test]
    fn test_audit_logger_creates_capture_dir() {
        let temp_dir = TempDir::new().unwrap();
        let log_path = temp_dir.path().join("audit.log");
        let capture_dir = temp_dir.path().join("nested/captures");

        let logger = AuditLogger::new(&log_path, Some(capture_dir.clone()));
        logger
            .save_full_capture(&pane_id("debug-1"), "content")
            .unwrap();

        assert!(capture_dir.exists());
    }

    // --- MaybeAuditLogger Tests ---

    #[test]
    fn test_maybe_audit_logger_disabled() {
        let logger = MaybeAuditLogger::disabled();

        assert!(!logger.is_enabled());

        // All operations should succeed silently
        logger
            .log_create_pane(&pane_id("debug-1"), "cargo run", None)
            .unwrap();
        logger.log_send_keys(&pane_id("debug-1"), "test").unwrap();
        logger
            .log_capture_pane(&pane_id("debug-1"), 100, 5000)
            .unwrap();
        logger.log_kill_pane(&pane_id("debug-1")).unwrap();
        logger.log_list_panes().unwrap();
    }

    #[test]
    fn test_maybe_audit_logger_enabled() {
        let temp_dir = TempDir::new().unwrap();
        let log_path = temp_dir.path().join("audit.log");

        let inner = AuditLogger::new(&log_path, None);
        let logger = MaybeAuditLogger::new(Some(inner));

        assert!(logger.is_enabled());

        logger
            .log_create_pane(&pane_id("debug-1"), "cargo run", Some("server"))
            .unwrap();

        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("create_pane"));
    }

    #[test]
    fn test_maybe_audit_logger_save_full_capture_disabled() {
        let logger = MaybeAuditLogger::disabled();

        let result = logger
            .save_full_capture(&pane_id("debug-1"), "content")
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_maybe_audit_logger_save_full_capture_enabled() {
        let temp_dir = TempDir::new().unwrap();
        let log_path = temp_dir.path().join("audit.log");
        let capture_dir = temp_dir.path().join("captures");

        let inner = AuditLogger::new(&log_path, Some(capture_dir));
        let logger = MaybeAuditLogger::new(Some(inner));

        let result = logger
            .save_full_capture(&pane_id("debug-1"), "content")
            .unwrap();
        assert!(result.is_some());
    }

    // --- Byte Count Tests (important for spec compliance) ---

    #[test]
    fn test_output_bytes_count_accuracy() {
        // Verify that output_bytes reflects actual byte length
        let content = "Hello, World! 🦀"; // Contains multi-byte character
        let byte_len = content.len(); // Should be > character count due to emoji

        let entry = AuditEntry::capture_pane(&pane_id("debug-1"), 100, byte_len);
        assert_eq!(entry.output_bytes, Some(byte_len));

        // Verify the byte count
        assert!(byte_len > 14, "Emoji should add extra bytes: {}", byte_len);
    }

    #[test]
    fn test_json_output_does_not_contain_full_content() {
        // Verify that the JSON log entry only contains byte count, not content
        let entry = AuditEntry::capture_pane(&pane_id("debug-1"), 100, 5000);
        let json = entry.to_json().unwrap();

        // Should have output_bytes
        assert!(json.contains("output_bytes"));
        assert!(json.contains("5000"));

        // Should NOT have any field that might contain captured content
        // (there's no such field in the struct, but let's verify the JSON)
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("content").is_none());
        assert!(parsed.get("output").is_none());
        assert!(parsed.get("text").is_none());
    }
}
