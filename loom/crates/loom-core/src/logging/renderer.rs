use std::io::{self, Write};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::agent::AgentEvent;
use crate::identifier::{BeadId, ProfileName};

/// Final outcome of a bead spawn — drives the closing line color and glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeadOutcome {
    /// Agent finished cleanly; closing line uses green ✓.
    Done,
    /// Agent failed; closing line uses red ✗.
    Failed,
    /// Run is being retried with previous-error context; line uses yellow.
    Retry,
}

/// Output verbosity. `Default` prints one short line per tool call;
/// `Verbose` additionally streams assistant text deltas and tool args inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Default,
    Verbose,
}

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_YELLOW: &str = "\x1b[33m";

/// Per-bead terminal renderer.
///
/// One renderer is constructed per bead spawn. The driver creates it via
/// [`TerminalRenderer::new`], emits the header, drives [`render_event`] for
/// every [`AgentEvent`], and finally emits the closing line via
/// [`finish`]. With `--parallel N > 1`, the renderer is constructed in
/// `Parallel` mode so tool-call lines carry a `[bead-id]` prefix for
/// attribution.
pub struct TerminalRenderer {
    out: Box<dyn Write + Send>,
    mode: RenderMode,
    bead_id: BeadId,
    parallel: bool,
    color: bool,
    started: Instant,
    tool_count: u32,
    header_printed: bool,
    closed: bool,
}

impl TerminalRenderer {
    /// Build a renderer that writes to `out`. `parallel` controls whether
    /// tool-call lines are prefixed with `[<bead-id>]`. `color` enables ANSI
    /// status colors on the header/finish lines (tool-call lines stay plain
    /// for grep-friendliness).
    pub fn new(
        out: impl Write + Send + 'static,
        mode: RenderMode,
        bead_id: BeadId,
        parallel: bool,
        color: bool,
    ) -> Self {
        Self {
            out: Box::new(out),
            mode,
            bead_id,
            parallel,
            color,
            started: Instant::now(),
            tool_count: 0,
            header_printed: false,
            closed: false,
        }
    }

    /// Print the per-bead header line.
    ///
    /// `▸ <bead-id>  <title>    [profile:<name>]`
    ///
    /// Atomically printed (single write call) so parallel renderers do not
    /// interleave the header itself.
    pub fn header(&mut self, title: &str, profile: &ProfileName) -> io::Result<()> {
        let line = format!(
            "▸ {id}  {title}    [profile:{profile}]\n",
            id = self.bead_id.as_str(),
            title = title,
            profile = profile.as_str(),
        );
        self.out.write_all(line.as_bytes())?;
        self.out.flush()?;
        self.header_printed = true;
        Ok(())
    }

    /// Render one [`AgentEvent`].
    ///
    /// In `Default` mode: prints a one-line summary for `ToolCall` (name +
    /// truncated path/range/cmd-prefix). Suppresses `MessageDelta`,
    /// `ToolResult`, `TurnEnd`, and the compaction events.
    ///
    /// In `Verbose` mode: also writes `MessageDelta.text` straight through
    /// (no newline injection) and includes the tool-call args inline.
    pub fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        match event {
            AgentEvent::ToolCall { tool, params, .. } => {
                self.tool_count += 1;
                let summary = if matches!(self.mode, RenderMode::Verbose) {
                    format!("{} {}", short_summary(tool, params), inline_args(params))
                } else {
                    short_summary(tool, params)
                };
                let line = if self.parallel {
                    format!("  [{}] {summary}\n", self.bead_id.as_str())
                } else {
                    format!("  {summary}\n")
                };
                self.out.write_all(line.as_bytes())?;
                self.out.flush()?;
            }
            AgentEvent::MessageDelta { text } if matches!(self.mode, RenderMode::Verbose) => {
                self.out.write_all(text.as_bytes())?;
                self.out.flush()?;
            }
            AgentEvent::Error { message } => {
                let line = if self.parallel {
                    format!("  [{}] error: {message}\n", self.bead_id.as_str())
                } else {
                    format!("  error: {message}\n")
                };
                self.out.write_all(line.as_bytes())?;
                self.out.flush()?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Print the closing line and consume the renderer.
    ///
    /// `✓ done  (N tool calls, Ms)` for [`BeadOutcome::Done`],
    /// `✗ failed  (N tool calls, Ms)` for [`BeadOutcome::Failed`],
    /// `↻ retry  (N tool calls, Ms)` for [`BeadOutcome::Retry`].
    ///
    /// Status color is applied to the leading glyph + word only when
    /// `color = true`; the rest of the line is plain.
    pub fn finish(mut self, outcome: BeadOutcome) -> io::Result<()> {
        let elapsed = self.started.elapsed();
        self.write_finish(outcome, elapsed)
    }

    fn write_finish(&mut self, outcome: BeadOutcome, elapsed: Duration) -> io::Result<()> {
        let (glyph, word, color) = match outcome {
            BeadOutcome::Done => ("✓", "done", ANSI_GREEN),
            BeadOutcome::Failed => ("✗", "failed", ANSI_RED),
            BeadOutcome::Retry => ("↻", "retry", ANSI_YELLOW),
        };
        let prefix = if self.color {
            format!("{color}{glyph} {word}{ANSI_RESET}")
        } else {
            format!("{glyph} {word}")
        };
        let body = format!(
            "  ({tools} tool calls, {secs}s)\n",
            tools = self.tool_count,
            secs = elapsed.as_secs(),
        );
        let line = if self.parallel {
            format!("[{}] {prefix}{body}", self.bead_id.as_str())
        } else {
            format!("  {prefix}{body}")
        };
        self.out.write_all(line.as_bytes())?;
        self.out.flush()?;
        self.closed = true;
        Ok(())
    }

    /// Number of `ToolCall` events observed since construction.
    pub fn tool_count(&self) -> u32 {
        self.tool_count
    }

    /// Whether the header line has been printed.
    pub fn header_printed(&self) -> bool {
        self.header_printed
    }
}

/// Build the "<tool> <short-arg>" summary used by both modes.
///
/// - `Read`/`Edit`/`Write` use the `file_path` field, optionally suffixed with
///   the line range (`+<offset>-<limit>`).
/// - `Bash` uses the first 60 chars of `command`.
/// - Anything else falls back to just the tool name.
fn short_summary(tool: &str, params: &Value) -> String {
    let arg: Option<String> = match tool {
        "Read" => path_with_range(params, "file_path", "offset", "limit"),
        "Edit" | "Write" => path_summary(params, "file_path"),
        "Bash" => bash_summary(params),
        "Grep" => grep_summary(params),
        _ => None,
    };
    arg.map(|a| format!("{tool:<7} {a}"))
        .unwrap_or_else(|| format!("{tool:<7}"))
}

fn path_summary(params: &Value, field: &str) -> Option<String> {
    params
        .get(field)
        .and_then(Value::as_str)
        .map(truncate_one_line)
}

fn path_with_range(
    params: &Value,
    path_field: &str,
    offset_field: &str,
    limit_field: &str,
) -> Option<String> {
    let path = params.get(path_field).and_then(Value::as_str)?;
    let offset = params.get(offset_field).and_then(Value::as_u64);
    let limit = params.get(limit_field).and_then(Value::as_u64);
    let suffix = match (offset, limit) {
        (Some(o), Some(l)) => format!(" +{o}-{l}"),
        (Some(o), None) => format!(" +{o}"),
        _ => String::new(),
    };
    Some(format!("{}{suffix}", truncate_one_line(path)))
}

fn bash_summary(params: &Value) -> Option<String> {
    let cmd = params.get("command").and_then(Value::as_str)?;
    Some(truncate_to(cmd, 60))
}

fn grep_summary(params: &Value) -> Option<String> {
    let pattern = params.get("pattern").and_then(Value::as_str)?;
    let path = params
        .get("path")
        .and_then(Value::as_str)
        .map(|p| format!(" in {p}"))
        .unwrap_or_default();
    Some(format!("{}{path}", truncate_to(pattern, 40)))
}

/// Render the full args dict on one line for `--verbose` tool-call lines.
fn inline_args(params: &Value) -> String {
    let s = serde_json::to_string(params).unwrap_or_default();
    truncate_to(&s, 200)
}

fn truncate_one_line(s: &str) -> String {
    truncate_to(s, 80)
}

fn truncate_to(s: &str, max: usize) -> String {
    let cleaned: String = s.chars().take_while(|c| *c != '\n').collect();
    if cleaned.len() <= max {
        cleaned
    } else {
        let mut out: String = cleaned.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use crate::identifier::ToolCallId;
    use serde_json::json;

    fn capture<F>(mode: RenderMode, parallel: bool, color: bool, f: F) -> String
    where
        F: FnOnce(&mut TerminalRenderer),
    {
        let buf: Vec<u8> = Vec::new();
        let cell = std::sync::Arc::new(std::sync::Mutex::new(buf));
        let cell_for_writer = cell.clone();
        struct Sink {
            inner: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        }
        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.inner
                    .lock()
                    .map_err(|_| io::Error::other("poisoned"))?
                    .extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut r = TerminalRenderer::new(
            Sink {
                inner: cell_for_writer,
            },
            mode,
            BeadId::new("wx-1").expect("valid bead id"),
            parallel,
            color,
        );
        f(&mut r);
        let guard = cell.lock().expect("not poisoned");
        String::from_utf8(guard.clone()).expect("utf-8")
    }

    #[test]
    fn default_mode_suppresses_message_deltas() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::MessageDelta {
                text: "hello world".to_string(),
            })
            .expect("render");
        });
        assert_eq!(out, "");
    }

    #[test]
    fn verbose_mode_streams_message_deltas_verbatim() {
        let out = capture(RenderMode::Verbose, false, false, |r| {
            r.render_event(&AgentEvent::MessageDelta {
                text: "hel".to_string(),
            })
            .expect("render");
            r.render_event(&AgentEvent::MessageDelta {
                text: "lo".to_string(),
            })
            .expect("render");
        });
        assert_eq!(out, "hello");
    }

    #[test]
    fn tool_call_line_in_default_mode_shows_path() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                id: ToolCallId::new("t1"),
                tool: "Read".to_string(),
                params: json!({"file_path": "src/lib.rs"}),
            })
            .expect("render");
        });
        assert!(out.contains("Read"), "{out:?}");
        assert!(out.contains("src/lib.rs"), "{out:?}");
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn parallel_mode_prefixes_tool_lines_with_bead_id() {
        let out = capture(RenderMode::Default, true, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                id: ToolCallId::new("t1"),
                tool: "Bash".to_string(),
                params: json!({"command": "cargo build"}),
            })
            .expect("render");
        });
        assert!(out.contains("[wx-1]"), "{out:?}");
        assert!(out.contains("Bash"));
    }

    #[test]
    fn header_line_includes_id_title_profile() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.header("Implement parser", &ProfileName::new("rust"))
                .expect("header");
        });
        assert!(out.contains("wx-1"), "{out:?}");
        assert!(out.contains("Implement parser"));
        assert!(out.contains("[profile:rust]"));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn finish_done_includes_tool_count_and_secs() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                id: ToolCallId::new("t1"),
                tool: "Read".to_string(),
                params: json!({"file_path": "a"}),
            })
            .expect("render");
            r.render_event(&AgentEvent::ToolCall {
                id: ToolCallId::new("t2"),
                tool: "Edit".to_string(),
                params: json!({"file_path": "b"}),
            })
            .expect("render");
            r.write_finish(BeadOutcome::Done, std::time::Duration::from_secs(47))
                .expect("finish");
        });
        assert!(out.contains("done"));
        assert!(out.contains("2 tool calls"));
        assert!(out.contains("47s"));
    }

    #[test]
    fn finish_with_color_emits_ansi_codes() {
        let out = capture(RenderMode::Default, false, true, |r| {
            r.write_finish(BeadOutcome::Failed, std::time::Duration::from_secs(1))
                .expect("finish");
        });
        assert!(out.contains(ANSI_RED), "{out:?}");
        assert!(out.contains(ANSI_RESET));
    }

    #[test]
    fn truncate_collapses_newlines_to_first_line() {
        assert_eq!(truncate_to("a\nb\nc", 10), "a");
    }

    #[test]
    fn truncate_to_caps_long_input_with_ellipsis() {
        let s = truncate_to("0123456789ABCDEF", 8);
        assert_eq!(s.chars().count(), 8);
        assert!(s.ends_with('…'));
    }
}
