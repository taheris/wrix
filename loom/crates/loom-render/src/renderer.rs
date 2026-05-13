use std::io::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::clock::{Clock, SystemClock};
use loom_events::identifier::{BeadId, ProfileName};
use loom_events::{AgentEvent, EventEnvelope};

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

/// Output mode for the renderer trait. Drives the per-impl selection
/// in [`select_renderer`] and the CLI flag wiring in `loom run` /
/// `loom logs`. Mode is the format dimension; verbosity is orthogonal.
///
/// Selection logic per spec (`H2` table):
/// - TTY + no `--plain`/`--json`/`--raw` + no `NO_COLOR` → `Pretty`
/// - non-TTY OR `NO_COLOR` set OR `--plain` → `Plain`
/// - `--json` → `Json`
/// - `--raw` → `Raw`
///
/// `--json` and `--raw` are mutually exclusive; `--raw` is mutually
/// exclusive with `-v/--verbose`. The CLI surface enforces these via
/// clap `conflicts_with`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Internal "verbose pretty" — kept so the existing per-bead
    /// `TerminalRenderer` path stays compatible. New code should
    /// consume the [`Renderer`] trait, not match on this.
    Default,
    /// Internal "verbose pretty" — same caveat.
    Verbose,
    /// Colored, glyph-decorated output for an interactive terminal.
    Pretty,
    /// ASCII glyphs, no color, no OSC 8 — pipe-safe.
    Plain,
    /// One pretty-printed JSON object per line; pure data, no chrome.
    Json,
    /// Passthrough JSONL: each event serialized as one compact JSON
    /// line. Used by `loom logs --raw` to reproduce the on-disk shape.
    Raw,
}

impl RenderMode {
    /// Auto-select the right mode for a given (TTY, flags, env) state.
    /// Pure function — every input is a parameter so tests can pin
    /// behavior without env mutation.
    pub fn select(
        tty: bool,
        no_color_env: bool,
        flag_plain: bool,
        flag_json: bool,
        flag_raw: bool,
    ) -> RenderMode {
        if flag_raw {
            RenderMode::Raw
        } else if flag_json {
            RenderMode::Json
        } else if flag_plain || !tty || no_color_env {
            RenderMode::Plain
        } else {
            RenderMode::Pretty
        }
    }
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
    clock: Arc<dyn Clock>,
    started: Instant,
    tool_count: u32,
    header_printed: bool,
    closed: bool,
}

impl TerminalRenderer {
    /// Build a renderer that writes to `out` using a [`SystemClock`] for the
    /// elapsed-time line at finish.
    pub fn new(
        out: impl Write + Send + 'static,
        mode: RenderMode,
        bead_id: BeadId,
        parallel: bool,
        color: bool,
    ) -> Self {
        Self::with_clock(out, mode, bead_id, parallel, color, SystemClock::new())
    }

    /// Build a renderer with an explicit clock. Tests inject
    /// [`crate::clock::MockClock`] so the elapsed-time output is deterministic
    /// without wall-clock dependence.
    pub fn with_clock(
        out: impl Write + Send + 'static,
        mode: RenderMode,
        bead_id: BeadId,
        parallel: bool,
        color: bool,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let started = clock.now();
        Self {
            out: Box::new(out),
            mode,
            bead_id,
            parallel,
            color,
            clock,
            started,
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
            AgentEvent::TextDelta { text, .. } if matches!(self.mode, RenderMode::Verbose) => {
                self.out.write_all(text.as_bytes())?;
                self.out.flush()?;
            }
            AgentEvent::Error { message, .. } => {
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
        let elapsed = self.clock.now().saturating_duration_since(self.started);
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

/// Output target for one bead's event stream. Four concrete impls
/// (Pretty/Plain/Json/Raw) sit behind this trait; selection happens
/// once per spawn via [`RenderMode::select`] and the chosen impl is
/// handed to [`crate::LogSink`] as `Box<dyn Renderer>`.
///
/// `loom run` and `loom logs` share these impls — the `live` /
/// replay distinction is encoded in how they feed events to the
/// renderer, not in the renderer itself. H3-H6 add per-tool body
/// rendering, in-place running indicators, OSC 8 hyperlinks, and
/// Task subagent nesting on top of this trait.
pub trait Renderer: Send {
    /// Optional per-bead header — `Pretty`/`Plain` print it; `Json`/`Raw`
    /// suppress (header is chrome, not data). Default impl is a no-op
    /// so simpler impls don't have to override.
    fn header(&mut self, _title: &str, _profile: &ProfileName) -> io::Result<()> {
        Ok(())
    }

    /// Render one event. The renderer owns its `Write` sink.
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()>;

    /// Close the renderer with the bead outcome. `&mut self` (not
    /// consuming) so trait-object call through `Box<dyn Renderer>`
    /// stays straightforward without `Box<Self>` indirection.
    fn finish(&mut self, outcome: BeadOutcome, elapsed: Duration) -> io::Result<()>;
}

/// `Pretty` mode — colored, glyph-decorated output for an interactive
/// terminal. Thin wrapper that delegates to the existing
/// [`TerminalRenderer`] with `color = true`.
pub struct PrettyRenderer {
    inner: TerminalRenderer,
}

impl PrettyRenderer {
    pub fn new(out: impl Write + Send + 'static, bead_id: BeadId, parallel: bool) -> Self {
        Self {
            inner: TerminalRenderer::new(out, RenderMode::Default, bead_id, parallel, true),
        }
    }
}

impl Renderer for PrettyRenderer {
    fn header(&mut self, title: &str, profile: &ProfileName) -> io::Result<()> {
        self.inner.header(title, profile)
    }
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        self.inner.render_event(event)
    }
    fn finish(&mut self, outcome: BeadOutcome, elapsed: Duration) -> io::Result<()> {
        self.inner.write_finish(outcome, elapsed)
    }
}

/// `Plain` mode — same shape as `Pretty` but with `color = false`.
/// Pipe-safe; no ANSI escapes; no OSC 8.
pub struct PlainRenderer {
    inner: TerminalRenderer,
}

impl PlainRenderer {
    pub fn new(out: impl Write + Send + 'static, bead_id: BeadId, parallel: bool) -> Self {
        Self {
            inner: TerminalRenderer::new(out, RenderMode::Default, bead_id, parallel, false),
        }
    }
}

impl Renderer for PlainRenderer {
    fn header(&mut self, title: &str, profile: &ProfileName) -> io::Result<()> {
        self.inner.header(title, profile)
    }
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        self.inner.render_event(event)
    }
    fn finish(&mut self, outcome: BeadOutcome, elapsed: Duration) -> io::Result<()> {
        self.inner.write_finish(outcome, elapsed)
    }
}

/// `Json` mode — one pretty-printed JSON object per line. No header,
/// no closing line; the consumer is expected to parse the stream.
pub struct JsonRenderer {
    out: Box<dyn Write + Send>,
}

impl JsonRenderer {
    pub fn new(out: impl Write + Send + 'static) -> Self {
        Self { out: Box::new(out) }
    }
}

impl Renderer for JsonRenderer {
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        let line = serde_json::to_string_pretty(event)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.out.write_all(line.as_bytes())?;
        self.out.write_all(b"\n")?;
        self.out.flush()
    }
    fn finish(&mut self, _outcome: BeadOutcome, _elapsed: Duration) -> io::Result<()> {
        self.out.flush()
    }
}

/// `Raw` mode — one compact JSON line per event. Used by
/// `loom logs --raw` to reproduce the on-disk shape exactly.
pub struct RawRenderer {
    out: Box<dyn Write + Send>,
}

impl RawRenderer {
    pub fn new(out: impl Write + Send + 'static) -> Self {
        Self { out: Box::new(out) }
    }
}

impl Renderer for RawRenderer {
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        let line = serde_json::to_string(event)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.out.write_all(line.as_bytes())?;
        self.out.write_all(b"\n")?;
        self.out.flush()
    }
    fn finish(&mut self, _outcome: BeadOutcome, _elapsed: Duration) -> io::Result<()> {
        self.out.flush()
    }
}

/// Construct the right [`Renderer`] for a given mode. Production
/// callers in `loom run` / `loom logs` pass a `Box<dyn Write + Send>`
/// (typically `io::stdout()` or a buffered wrapper).
pub fn build_renderer(
    mode: RenderMode,
    out: Box<dyn Write + Send>,
    bead_id: BeadId,
    parallel: bool,
) -> Box<dyn Renderer> {
    match mode {
        RenderMode::Pretty => Box::new(PrettyRenderer::new(out, bead_id, parallel)),
        RenderMode::Plain | RenderMode::Default | RenderMode::Verbose => {
            Box::new(PlainRenderer::new(out, bead_id, parallel))
        }
        RenderMode::Json => Box::new(JsonRenderer::new(out)),
        RenderMode::Raw => Box::new(RawRenderer::new(out)),
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use loom_events::identifier::ToolCallId;
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
            r.render_event(&AgentEvent::TextDelta {
                envelope: EventEnvelope::default(),
                text: "hello world".to_string(),
            })
            .expect("render");
        });
        assert_eq!(out, "");
    }

    #[test]
    fn verbose_mode_streams_message_deltas_verbatim() {
        let out = capture(RenderMode::Verbose, false, false, |r| {
            r.render_event(&AgentEvent::TextDelta {
                envelope: EventEnvelope::default(),
                text: "hel".to_string(),
            })
            .expect("render");
            r.render_event(&AgentEvent::TextDelta {
                envelope: EventEnvelope::default(),
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
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("t1"),
                tool: "Read".to_string(),
                params: json!({"file_path": "src/lib.rs"}),
                parent_tool_call_id: None,
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
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("t1"),
                tool: "Bash".to_string(),
                params: json!({"command": "cargo build"}),
                parent_tool_call_id: None,
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
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("t1"),
                tool: "Read".to_string(),
                params: json!({"file_path": "a"}),
                parent_tool_call_id: None,
            })
            .expect("render");
            r.render_event(&AgentEvent::ToolCall {
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("t2"),
                tool: "Edit".to_string(),
                params: json!({"file_path": "b"}),
                parent_tool_call_id: None,
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

    // -- H2 tests ----------------------------------------------------------

    fn captured() -> (
        std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        impl Write + Send + 'static,
    ) {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = SharedWriter(buf.clone());
        (buf, writer)
    }

    struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| io::Error::other("poisoned"))?
                .extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn captured_str(buf: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>) -> String {
        let g = buf.lock().expect("not poisoned");
        String::from_utf8(g.clone()).expect("utf-8")
    }

    fn sample_text_delta() -> AgentEvent {
        AgentEvent::TextDelta {
            envelope: EventEnvelope::default(),
            text: "hello".into(),
        }
    }

    fn sample_tool_call() -> AgentEvent {
        AgentEvent::ToolCall {
            envelope: EventEnvelope::default(),
            id: ToolCallId::new("t1"),
            tool: "Read".into(),
            params: json!({"file_path": "src/lib.rs"}),
            parent_tool_call_id: None,
        }
    }

    /// All four spec'd render modes are present and `build_renderer`
    /// dispatches to a concrete impl for each.
    #[test]
    fn renderer_modes_present() {
        let bead = BeadId::new("wx-1").expect("valid id");
        for mode in [
            RenderMode::Pretty,
            RenderMode::Plain,
            RenderMode::Json,
            RenderMode::Raw,
        ] {
            let (_, writer) = captured();
            let r = build_renderer(mode, Box::new(writer), bead.clone(), false);
            drop(r); // sanity — each mode constructs without panic
        }
    }

    /// TTY + no env / flags → Pretty.
    #[test]
    fn pretty_selected_on_tty() {
        assert_eq!(
            RenderMode::select(true, false, false, false, false),
            RenderMode::Pretty,
        );
    }

    /// Non-TTY → Plain (NO_COLOR-agnostic).
    #[test]
    fn plain_selected_on_non_tty() {
        assert_eq!(
            RenderMode::select(false, false, false, false, false),
            RenderMode::Plain,
        );
    }

    /// `NO_COLOR=1` on TTY → Plain (no-color.org contract).
    #[test]
    fn plain_selected_when_no_color_env() {
        assert_eq!(
            RenderMode::select(true, true, false, false, false),
            RenderMode::Plain,
        );
    }

    /// `--json` beats TTY-detect → Json regardless of env.
    #[test]
    fn json_flag_wins_over_tty() {
        assert_eq!(
            RenderMode::select(true, false, false, true, false),
            RenderMode::Json,
        );
    }

    /// `--raw` beats `--json` per the spec's exclusivity contract.
    /// (clap enforces this at parse time but the selector is
    /// defensive: raw > json > plain > pretty.)
    #[test]
    fn raw_flag_wins_over_json() {
        assert_eq!(
            RenderMode::select(true, false, false, true, true),
            RenderMode::Raw,
        );
    }

    /// `JsonRenderer` writes pretty-printed JSON, one object per line.
    /// Pretty-printing means newlines INSIDE the object, then the
    /// terminator newline.
    #[test]
    fn json_mode_pretty_prints() {
        let (buf, writer) = captured();
        let mut r = JsonRenderer::new(writer);
        r.render_event(&sample_text_delta()).expect("render");
        let out = captured_str(&buf);
        assert!(out.starts_with('{'), "json must start with `{{`: {out}");
        // Pretty-printing keeps fields on separate lines.
        assert!(
            out.contains("\n  \"kind\""),
            "json output must be pretty-printed: {out}",
        );
        assert!(out.ends_with('\n'));
    }

    /// `RawRenderer` writes a compact JSON line — no embedded newlines
    /// in the object, terminator newline at end. Round-trips back to
    /// `AgentEvent` cleanly.
    #[test]
    fn raw_mode_passthrough() {
        let (buf, writer) = captured();
        let mut r = RawRenderer::new(writer);
        r.render_event(&sample_tool_call()).expect("render");
        let out = captured_str(&buf);
        let trimmed = out.trim_end_matches('\n');
        // Compact form has no internal newlines.
        assert!(
            !trimmed.contains('\n'),
            "raw output must be one compact line: {out}",
        );
        // Parses back to the same variant.
        let back: AgentEvent = serde_json::from_str(trimmed).expect("parse back");
        assert!(matches!(back, AgentEvent::ToolCall { .. }));
    }
}
