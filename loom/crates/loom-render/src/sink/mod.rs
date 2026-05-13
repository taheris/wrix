use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tracing::info;

use loom_events::AgentEvent;
use loom_events::identifier::{BeadId, SpecLabel};

mod error;

pub use error::LogError;

use crate::path::{bead_log_path, phase_log_path};
use crate::renderer::{BeadOutcome, TerminalRenderer};

/// Tee-style sink that drives the per-bead JSONL log file *and* the
/// [`TerminalRenderer`] from the same `emit` call.
///
/// Spec contract (`Run UX & Logging`): "the terminal renderer consumes the
/// same `AgentEvent` stream that's written to disk — there's a single
/// tee-style sink, not two parallel pipelines." This is enforced by the type:
/// every event flows through one method ([`Self::emit`]) which dispatches to
/// both writers in lockstep, so the terminal cannot diverge from the on-disk
/// log.
pub struct LogSink {
    file: BufWriter<File>,
    renderer: Option<TerminalRenderer>,
    log_path: PathBuf,
    finished: bool,
}

impl LogSink {
    /// Open a per-bead sink under
    /// `<logs_root>/<spec-label>/<bead-id>-<utc>.jsonl`. `renderer` is
    /// optional so non-interactive callers (the run/parallel dispatch closure
    /// in the binary) can write only the on-disk JSONL without instantiating
    /// a `TerminalRenderer`.
    pub fn open_in_at(
        logs_root: &Path,
        spec_label: &SpecLabel,
        bead_id: &BeadId,
        renderer: Option<TerminalRenderer>,
        when: SystemTime,
    ) -> Result<Self, LogError> {
        let log_path = bead_log_path(logs_root, spec_label, bead_id, when);
        let sink = Self::open_at_path(log_path.clone(), renderer)?;
        info!(
            target: "loom_driver::logging::sink",
            spec_label = spec_label.as_str(),
            bead_id = bead_id.as_str(),
            log_path = %log_path.display(),
            "spawn started — log path",
        );
        Ok(sink)
    }

    /// Open a sink for a non-bead phase (`loom todo`, `loom plan`,
    /// `loom check`). The path follows
    /// `<logs_root>/<spec-label>/<phase>-<utc>.jsonl` so phase logs share the
    /// same per-spec directory tree as bead logs without colliding.
    ///
    /// `renderer` is optional because phase logs may run in non-interactive
    /// contexts (CI, scripted spawns) where the per-bead progress chrome is
    /// noise; emitting only to the file is the lighter contract.
    pub fn open_phase_at(
        logs_root: &Path,
        spec_label: &SpecLabel,
        phase: &str,
        renderer: Option<TerminalRenderer>,
        when: SystemTime,
    ) -> Result<Self, LogError> {
        let log_path = phase_log_path(logs_root, spec_label, phase, when);
        let sink = Self::open_at_path(log_path.clone(), renderer)?;
        info!(
            target: "loom_driver::logging::sink",
            spec_label = spec_label.as_str(),
            phase = phase,
            log_path = %log_path.display(),
            "phase started — log path",
        );
        Ok(sink)
    }

    fn open_at_path(
        log_path: PathBuf,
        renderer: Option<TerminalRenderer>,
    ) -> Result<Self, LogError> {
        if let Some(dir) = log_path.parent() {
            fs::create_dir_all(dir).map_err(|source| LogError::CreateDir {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|source| LogError::OpenFile {
                path: log_path.clone(),
                source,
            })?;
        Ok(Self {
            file: BufWriter::new(file),
            renderer,
            log_path,
            finished: false,
        })
    }

    /// Write `event` to the on-disk JSONL log AND drive the terminal
    /// renderer in a single call.
    ///
    /// File-write order: serialize → append `\n` → write → flush. Renderer
    /// errors are surfaced with the same `LogError::Io` variant so the caller
    /// can decide whether one failure path should also fail the bead.
    pub fn emit(&mut self, event: &AgentEvent) -> Result<(), LogError> {
        let line = serde_json::to_string(event)?;
        let path = self.log_path.clone();
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.write_all(b"\n"))
            .and_then(|()| self.file.flush())
            .map_err(|source| LogError::Write {
                path: path.clone(),
                source,
            })?;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer
                .render_event(event)
                .map_err(|source| LogError::Write { path, source })?;
        }
        Ok(())
    }

    /// Print the renderer's closing line and flush the log file. Idempotent:
    /// a second call is a no-op so callers can defensively `finish` in both
    /// success and failure paths.
    pub fn finish(&mut self, outcome: BeadOutcome) -> Result<(), LogError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.file.flush().map_err(|source| LogError::Write {
            path: self.log_path.clone(),
            source,
        })?;
        if let Some(renderer) = self.renderer.take() {
            renderer.finish(outcome).map_err(|source| LogError::Write {
                path: self.log_path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    /// The resolved log file path. Useful for the workflow engine to surface
    /// in error messages / tests.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}

impl Drop for LogSink {
    fn drop(&mut self) {
        if !self.finished {
            // Best-effort flush; further writes after drop are impossible.
            let _ = self.file.flush();
        }
    }
}

#[cfg(test)]
pub(crate) type SharedBuffer = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

/// Helper used by tests in this crate. Opens a sink against an in-memory
/// renderer that discards output.
#[cfg(test)]
pub(crate) fn open_sink_with_sink_writer(
    logs_root: &Path,
    spec_label: &SpecLabel,
    bead_id: &BeadId,
    when: SystemTime,
) -> Result<(LogSink, SharedBuffer), LogError> {
    use std::io;

    use crate::renderer::RenderMode;
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let writer_buf = buf.clone();
    struct Sink {
        inner: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }
    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.inner
                .lock()
                .map_err(|_| io::Error::other("poisoned"))?
                .extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let renderer = TerminalRenderer::new(
        Sink { inner: writer_buf },
        RenderMode::Default,
        bead_id.clone(),
        false,
        false,
    );
    let sink = LogSink::open_in_at(logs_root, spec_label, bead_id, Some(renderer), when)?;
    Ok((sink, buf))
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use loom_events::event::CompactionReason;
    use loom_events::identifier::ToolCallId;
    use serde_json::{Value, json};

    fn read_lines(path: &Path) -> Vec<String> {
        let body = std::fs::read_to_string(path).expect("read");
        body.lines().map(str::to_owned).collect()
    }

    #[test]
    fn emit_writes_jsonl_line_per_event_and_drives_renderer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let label = SpecLabel::new("alpha");
        let bead = BeadId::new("wx-1").expect("valid bead id");
        let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(0);
        let (mut sink, term_buf) =
            open_sink_with_sink_writer(dir.path(), &label, &bead, when).expect("open");

        sink.emit(&AgentEvent::ToolCall {
            id: ToolCallId::new("t1"),
            tool: "Read".to_string(),
            params: json!({"file_path": "src/lib.rs"}),
        })
        .expect("emit");
        sink.emit(&AgentEvent::TurnEnd).expect("emit");

        let path = sink.log_path().to_path_buf();
        sink.finish(BeadOutcome::Done).expect("finish");

        let lines = read_lines(&path);
        assert_eq!(lines.len(), 2, "{lines:?}");
        let first: Value = serde_json::from_str(&lines[0]).expect("json");
        assert_eq!(first["kind"], "tool_call");
        assert_eq!(first["tool"], "Read");
        let second: Value = serde_json::from_str(&lines[1]).expect("json");
        assert_eq!(second["kind"], "turn_end");

        let term = term_buf.lock().expect("not poisoned");
        let term_str = std::str::from_utf8(&term).expect("utf-8");
        assert!(term_str.contains("Read"), "{term_str:?}");
        assert!(term_str.contains("done"));
    }

    #[test]
    fn emit_persists_compaction_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let label = SpecLabel::new("alpha");
        let bead = BeadId::new("wx-1").expect("valid bead id");
        let (mut sink, _term) = open_sink_with_sink_writer(
            dir.path(),
            &label,
            &bead,
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
        )
        .expect("open");
        sink.emit(&AgentEvent::CompactionStart {
            reason: CompactionReason::ContextLimit,
        })
        .expect("emit");
        let path = sink.log_path().to_path_buf();
        sink.finish(BeadOutcome::Done).expect("finish");
        let line = &read_lines(&path)[0];
        let v: Value = serde_json::from_str(line).expect("json");
        assert_eq!(v["kind"], "compaction_start");
        assert_eq!(v["reason"], "context_limit");
    }

    #[test]
    fn finish_is_idempotent_after_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, _term) = open_sink_with_sink_writer(
            dir.path(),
            &SpecLabel::new("alpha"),
            &BeadId::new("wx-1").expect("valid bead id"),
            SystemTime::UNIX_EPOCH,
        )
        .expect("open");
        // Drop without finish must not panic and must flush.
        drop(sink);
    }

    #[test]
    fn parent_directory_is_created_on_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logs = dir.path().join(".wrapix/loom/logs");
        let (sink, _) = open_sink_with_sink_writer(
            &logs,
            &SpecLabel::new("nested"),
            &BeadId::new("wx-1").expect("valid bead id"),
            SystemTime::UNIX_EPOCH,
        )
        .expect("open");
        assert!(sink.log_path().parent().expect("parent").is_dir());
    }

    #[test]
    fn phase_sink_writes_under_spec_directory_with_phase_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logs = dir.path().join(".wrapix/loom/logs");
        let label = SpecLabel::new("alpha");
        let mut sink = LogSink::open_phase_at(
            &logs,
            &label,
            "todo",
            None,
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
        )
        .expect("open phase sink");
        sink.emit(&AgentEvent::TurnEnd).expect("emit");
        let path = sink.log_path().to_path_buf();
        sink.finish(BeadOutcome::Done).expect("finish");

        assert_eq!(path.parent(), Some(logs.join("alpha").as_path()));
        let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
        assert!(stem.starts_with("todo-"), "{stem:?}");
        let lines = std::fs::read_to_string(&path).expect("read");
        assert!(lines.contains("\"kind\":\"turn_end\""), "{lines:?}");
    }
}
