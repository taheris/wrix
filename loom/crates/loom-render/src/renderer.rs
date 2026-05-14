use std::io::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::clock::{Clock, SystemClock};
use crate::in_place::CLEAR_TO_EOL;
use crate::tool_body;
use loom_events::AgentEvent;
use loom_events::identifier::{BeadId, ProfileName, ToolCallId};

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

/// Convert a `ts_ms` delta (signed millis) to a non-negative
/// [`Duration`]. Negative deltas (clock skew across producers, malformed
/// logs) saturate to zero so the renderer never panics on `from_millis`.
fn ts_ms_delta(start: i64, end: i64) -> Duration {
    let delta = end.saturating_sub(start);
    Duration::from_millis(u64::try_from(delta).unwrap_or(0))
}

/// State captured at `ToolCall` time so the matching `ToolResult` can
/// finalize the pair with the same context. The spec calls this out
/// explicitly as the `HashMap<ToolCallId, PendingToolCall>` that lets a
/// `tool_call` + `tool_result` collapse into one rendered block with a
/// duration computed from `envelope.ts_ms` deltas. The `tool`/`params`
/// fields are reserved for per-tool body formatting in verbose mode
/// (the spec's "Body (-v)" column); currently only `ts_ms` is read.
#[derive(Debug, Clone)]
struct PendingToolCall {
    #[expect(dead_code, reason = "reserved for per-tool verbose body formatting")]
    tool: String,
    #[expect(dead_code, reason = "reserved for per-tool verbose body formatting")]
    params: Value,
    /// `envelope.ts_ms` of the originating `ToolCall`. Used to compute
    /// the pair's duration on `ToolResult` without consulting the wall
    /// clock — same code path for live and replay.
    ts_ms: i64,
}

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
    /// Per-tool-call nesting depth. Populated when a `ToolCall` arrives
    /// — depth = parent's depth + 1 when `parent_tool_call_id` is set,
    /// 0 otherwise. The renderer indents the rendered line by
    /// `depth * 2` spaces (H6, wx-46jgi). Cleared lazily; entries only
    /// matter while the corresponding tool is in flight.
    indent_by_tool: std::collections::HashMap<loom_events::identifier::ToolCallId, usize>,
    /// Per-tool-call state captured at `ToolCall` time so the matching
    /// `ToolResult` can finalize the pair (R2, wx-k7tg5 + H2). Stores
    /// `(tool, params, ts_ms)` — params drives `cap_body` formatting in
    /// verbose mode, `ts_ms` lets the result line render its duration
    /// from the event envelope rather than wall clock.
    pending_calls: std::collections::HashMap<loom_events::identifier::ToolCallId, PendingToolCall>,
    /// In-place "running indicator" state for the top-level tool call
    /// currently in-flight (R3, wx-mpci2). When `Some((id, started,
    /// summary, indent))`, a `\r`-overwritable line has been written
    /// for that call and the matching `ToolResult` should finalize it.
    /// Any other event must clear/finalize the line first so it does
    /// not interleave with running-state text.
    running: Option<(ToolCallId, Instant, String, String)>,
    /// `true` for `loom run` (events arrive in real time) and `false`
    /// for `loom logs` replay. Replay mode suppresses the in-place
    /// running indicator entirely — every event already has its
    /// matching `ToolResult` on disk so the spinner has nothing to
    /// represent — and pair durations come from `ts_ms` deltas instead
    /// of the local clock.
    live: bool,
    /// `true` when the renderer should drive the in-place running
    /// indicator. Disabled in `Plain`/`Json`/`Raw` modes, when running
    /// in parallel (multiple `\r` regions don't compose), and when
    /// the writer is known to be non-TTY. The CLI surface decides this
    /// per spec (R5, wx-zorjk wires it through `RenderMode::select`).
    indicator_enabled: bool,
    /// OSC 8 hyperlink wrapping for path-bearing summary cells (R4,
    /// wx-iuw22). Default is disabled; callers in supported terminals
    /// enable via [`TerminalRenderer::with_osc8`].
    osc8: tool_body::Osc8Context,
    header_printed: bool,
    closed: bool,
}

impl TerminalRenderer {
    /// Build a renderer that writes to `out` using a [`SystemClock`] for the
    /// elapsed-time line at finish. Defaults to `live = true` — events
    /// stream from a running agent. Replay callers (`loom logs`) use
    /// [`TerminalRenderer::new_replay`] to flip the `live` bool.
    pub fn new(
        out: impl Write + Send + 'static,
        mode: RenderMode,
        bead_id: BeadId,
        parallel: bool,
        color: bool,
    ) -> Self {
        Self::with_clock(
            out,
            mode,
            bead_id,
            parallel,
            color,
            true,
            SystemClock::new(),
        )
    }

    /// Build a renderer in replay mode — events are read from a saved
    /// JSONL log rather than a running agent, so the in-place running
    /// indicator is suppressed and pair durations come from `ts_ms`
    /// deltas. The spec's "live: bool" parameter (H2/H6) lives here.
    pub fn new_replay(
        out: impl Write + Send + 'static,
        mode: RenderMode,
        bead_id: BeadId,
        parallel: bool,
        color: bool,
    ) -> Self {
        Self::with_clock(
            out,
            mode,
            bead_id,
            parallel,
            color,
            false,
            SystemClock::new(),
        )
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
        live: bool,
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
            indent_by_tool: std::collections::HashMap::new(),
            pending_calls: std::collections::HashMap::new(),
            running: None,
            // Indicator is enabled by default when color is on, we're
            // not in parallel mode, and events arrive live. Each
            // condition is a proxy for a real constraint: color → TTY,
            // !parallel → single \r-region on screen, live → there's
            // something to spin while we wait for. Tests pass
            // `color=false` to disable the indicator so captured-buffer
            // assertions stay stable.
            indicator_enabled: color && !parallel && live,
            live,
            osc8: tool_body::Osc8Context::disabled(),
            header_printed: false,
            closed: false,
        }
    }

    /// Enable OSC 8 hyperlink wrapping for path-bearing summary cells.
    /// Production callers in `loom run` probe `TERM_PROGRAM` / `TERM`
    /// via [`crate::osc8::supports_osc8`] and pass the workspace root
    /// as `cwd`. Tests pin both branches without env mutation by
    /// passing the boolean directly. Chainable; returns the renderer.
    pub fn with_osc8(mut self, ctx: tool_body::Osc8Context) -> Self {
        self.osc8 = ctx;
        self
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
            AgentEvent::ToolCall {
                envelope,
                id,
                tool,
                params,
                parent_tool_call_id,
            } => {
                // R3 — if another tool's running line is open, finalize
                // it before laying down a nested or sibling call.
                self.finalize_running_with_glyph("…")?;
                self.tool_count += 1;
                let depth = match parent_tool_call_id {
                    Some(parent) => self.indent_by_tool.get(parent).copied().unwrap_or(0) + 1,
                    None => 0,
                };
                self.indent_by_tool.insert(id.clone(), depth);
                self.pending_calls.insert(
                    id.clone(),
                    PendingToolCall {
                        tool: tool.clone(),
                        params: params.clone(),
                        ts_ms: envelope.ts_ms,
                    },
                );
                let summary = tool_body::summary_cell(tool, params, &self.osc8);
                let indent: String = "  ".repeat(depth);
                if self.indicator_enabled
                    && parent_tool_call_id.is_none()
                    && !matches!(self.mode, RenderMode::Verbose)
                {
                    // Default-mode + top-level + TTY: open the in-place
                    // running indicator. Subsequent events overwrite this
                    // line via `\r` + clear-to-EOL until ToolResult fires.
                    let head = format!("  {indent}{summary}");
                    let running_line = format!("{head}   running... 0.0s");
                    self.out.write_all(running_line.as_bytes())?;
                    self.out.flush()?;
                    self.running = Some((
                        id.clone(),
                        self.clock.now(),
                        summary.clone(),
                        indent.clone(),
                    ));
                } else {
                    let line = if self.parallel {
                        format!("  [{}] {indent}{summary}\n", self.bead_id.as_str())
                    } else {
                        format!("  {indent}{summary}\n")
                    };
                    self.out.write_all(line.as_bytes())?;
                    self.out.flush()?;
                }
            }
            AgentEvent::ToolResult {
                envelope,
                id,
                output,
                is_error,
            } => {
                // R3 — finalize the running indicator if this result
                // matches the open call. Non-matching results (which
                // shouldn't happen in practice but might under nesting)
                // close any open running line first to avoid orphaned
                // `\r` regions.
                let matches_running = self
                    .running
                    .as_ref()
                    .is_some_and(|(running_id, _, _, _)| running_id == id);
                let pair_duration = self
                    .pending_calls
                    .remove(id)
                    .map(|p| ts_ms_delta(p.ts_ms, envelope.ts_ms));
                if matches_running {
                    let glyph = if *is_error { "✗" } else { "✓" };
                    self.finalize_running_with_glyph_dur(glyph, pair_duration)?;
                } else {
                    self.finalize_running_with_glyph_dur("…", None)?;
                    // R3 + H2 — replay (non-indicator) path still needs
                    // to surface the pair's final glyph + duration so
                    // `loom logs` shows the same information as `loom
                    // run`. Emit one indented closer line under the
                    // already-printed tool-call summary.
                    if !self.live {
                        let glyph = if *is_error { "✗" } else { "✓" };
                        let depth = self.indent_by_tool.get(id).copied().unwrap_or(0);
                        let indent: String = "  ".repeat(depth + 1);
                        let secs = pair_duration.unwrap_or(Duration::ZERO).as_secs_f64();
                        let line = if self.parallel {
                            format!("  [{}] {indent}{glyph} {secs:.1}s\n", self.bead_id.as_str())
                        } else {
                            format!("  {indent}{glyph} {secs:.1}s\n")
                        };
                        self.out.write_all(line.as_bytes())?;
                        self.out.flush()?;
                    }
                }
                if matches!(self.mode, RenderMode::Verbose) {
                    let depth = self.indent_by_tool.get(id).copied().unwrap_or(0);
                    let indent: String = "  ".repeat(depth + 1);
                    let capped = tool_body::cap_body(output, self.bead_id.as_str(), id.as_str());
                    for body_line in capped.lines() {
                        let line = if self.parallel {
                            format!("  [{}] {indent}{body_line}\n", self.bead_id.as_str(),)
                        } else {
                            format!("{indent}{body_line}\n")
                        };
                        self.out.write_all(line.as_bytes())?;
                    }
                    if *is_error {
                        let marker = format!("{indent}[tool error]\n");
                        self.out.write_all(marker.as_bytes())?;
                    }
                    self.out.flush()?;
                }
            }
            AgentEvent::TextDelta { text, .. } if matches!(self.mode, RenderMode::Verbose) => {
                self.finalize_running_with_glyph("…")?;
                self.out.write_all(text.as_bytes())?;
                self.out.flush()?;
            }
            AgentEvent::Error { message, .. } => {
                self.finalize_running_with_glyph("✗")?;
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

    /// Close the in-place running line — if one is open — with a final
    /// `\r` + clear-to-EOL + `<summary>   <glyph> Ns\n` line. `glyph` is
    /// `✓` on clean ToolResult, `✗` on errored ToolResult, `…` when an
    /// intervening event preempted the result (a long-running tool
    /// that emitted unrelated state mid-flight). Idempotent — calling
    /// against `running = None` is a no-op so cleanup paths can call
    /// unconditionally.
    fn finalize_running_with_glyph(&mut self, glyph: &str) -> io::Result<()> {
        self.finalize_running_with_glyph_dur(glyph, None)
    }

    /// Same as [`finalize_running_with_glyph`] but lets the caller pass
    /// a duration derived from `ts_ms` deltas instead of falling back
    /// to the local clock. `loom logs` replay drives this with the
    /// event envelope's `ts_ms` so the rendered duration matches what
    /// the producer recorded, not how fast the replayer reads the file.
    fn finalize_running_with_glyph_dur(
        &mut self,
        glyph: &str,
        pair_duration: Option<Duration>,
    ) -> io::Result<()> {
        let Some((_, started, summary, indent)) = self.running.take() else {
            return Ok(());
        };
        let elapsed =
            pair_duration.unwrap_or_else(|| self.clock.now().saturating_duration_since(started));
        let secs = elapsed.as_secs_f64();
        self.out.write_all(b"\r")?;
        self.out.write_all(CLEAR_TO_EOL.as_bytes())?;
        let final_line = if self.parallel {
            format!(
                "  [{}] {indent}{summary}   {glyph} {secs:.1}s\n",
                self.bead_id.as_str(),
            )
        } else {
            format!("  {indent}{summary}   {glyph} {secs:.1}s\n")
        };
        self.out.write_all(final_line.as_bytes())?;
        self.out.flush()?;
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
        // R3 — close out any open running line before the bead's
        // closing summary so a cancelled / failed bead doesn't leave a
        // dangling `\r` region for the next renderer to inherit.
        let cleanup_glyph = match outcome {
            BeadOutcome::Done => "✓",
            BeadOutcome::Failed => "✗",
            BeadOutcome::Retry => "↻",
        };
        self.finalize_running_with_glyph(cleanup_glyph)?;
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

impl Drop for TerminalRenderer {
    /// Best-effort cleanup of any open in-place running line so a
    /// panicking / dropped renderer never leaves a dangling `\r`
    /// region. The explicit cleanup contract still lives in
    /// [`finish`] — Drop is the safety net.
    fn drop(&mut self) {
        if self.running.is_some() {
            let _ = self.finalize_running_with_glyph("⚠");
        }
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

impl Renderer for TerminalRenderer {
    fn header(&mut self, title: &str, profile: &ProfileName) -> io::Result<()> {
        TerminalRenderer::header(self, title, profile)
    }
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        TerminalRenderer::render_event(self, event)
    }
    fn finish(&mut self, outcome: BeadOutcome, elapsed: Duration) -> io::Result<()> {
        self.write_finish(outcome, elapsed)
    }
}

/// `Pretty` mode — colored, glyph-decorated output for an interactive
/// terminal. Thin wrapper that delegates to the existing
/// [`TerminalRenderer`] with `color = true`. `live` distinguishes
/// `loom run` (events arrive in real time, in-place indicator spins)
/// from `loom logs` (replay; indicator suppressed, durations from
/// `ts_ms` deltas).
pub struct PrettyRenderer {
    inner: TerminalRenderer,
}

impl PrettyRenderer {
    pub fn new(
        out: impl Write + Send + 'static,
        bead_id: BeadId,
        parallel: bool,
        live: bool,
    ) -> Self {
        let inner = if live {
            TerminalRenderer::new(out, RenderMode::Default, bead_id, parallel, true)
        } else {
            TerminalRenderer::new_replay(out, RenderMode::Default, bead_id, parallel, true)
        };
        Self { inner }
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
/// Pipe-safe; no ANSI escapes; no OSC 8. The `live` parameter follows
/// the same contract as [`PrettyRenderer::new`].
pub struct PlainRenderer {
    inner: TerminalRenderer,
}

impl PlainRenderer {
    pub fn new(
        out: impl Write + Send + 'static,
        bead_id: BeadId,
        parallel: bool,
        live: bool,
    ) -> Self {
        let inner = if live {
            TerminalRenderer::new(out, RenderMode::Default, bead_id, parallel, false)
        } else {
            TerminalRenderer::new_replay(out, RenderMode::Default, bead_id, parallel, false)
        };
        Self { inner }
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
/// (typically `io::stdout()` or a buffered wrapper). `live` is `true`
/// for `loom run` (in-place indicator + wall-clock fallback) and
/// `false` for `loom logs` replay (indicator suppressed, durations
/// from `ts_ms` deltas).
pub fn build_renderer(
    mode: RenderMode,
    out: Box<dyn Write + Send>,
    bead_id: BeadId,
    parallel: bool,
    live: bool,
) -> Box<dyn Renderer> {
    match mode {
        RenderMode::Pretty => Box::new(PrettyRenderer::new(out, bead_id, parallel, live)),
        RenderMode::Plain | RenderMode::Default => {
            Box::new(PlainRenderer::new(out, bead_id, parallel, live))
        }
        // `Verbose` is a real mode at the TerminalRenderer level — it
        // streams `TextDelta` verbatim and renders capped tool bodies.
        // Construct the underlying renderer with `RenderMode::Verbose`
        // directly so the verbose dispatch in `render_event` triggers;
        // `color=true` is safe because the writer is opaque (TTY-vs-pipe
        // is the caller's concern via `RenderMode::select`).
        RenderMode::Verbose => {
            let inner = if live {
                TerminalRenderer::new(out, RenderMode::Verbose, bead_id, parallel, true)
            } else {
                TerminalRenderer::new_replay(out, RenderMode::Verbose, bead_id, parallel, true)
            };
            Box::new(inner)
        }
        RenderMode::Json => Box::new(JsonRenderer::new(out)),
        RenderMode::Raw => Box::new(RawRenderer::new(out)),
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use loom_events::EventEnvelope;
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

    // -- R2 tests ----------------------------------------------------------

    /// `ToolCall` lines use the per-tool spec cell — `Read   <path>:<range>`,
    /// `Edit   <path>   +N -M   diff↓`, `Bash   <cmd>`. The cell shape comes
    /// from `tool_body::summary_cell`; this test pins that the renderer
    /// dispatches through it (R2, wx-k7tg5).
    #[test]
    fn tool_call_line_uses_summary_cell_shape() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("e1"),
                tool: "Edit".into(),
                params: json!({
                    "file_path": "src/lib.rs",
                    "old_string": "fn old() {}\n",
                    "new_string": "fn new() {}\nfn extra() {}\n",
                }),
                parent_tool_call_id: None,
            })
            .expect("render");
        });
        assert!(out.contains("Edit"), "{out:?}");
        assert!(out.contains("src/lib.rs"), "{out:?}");
        assert!(out.contains("+"), "{out:?}");
        assert!(out.contains("-"), "{out:?}");
        assert!(out.contains("diff"), "{out:?}");
    }

    /// Verbose mode emits the `ToolResult` body, capped to 10 lines, with
    /// the spec'd recovery hint when truncated.
    #[test]
    fn verbose_mode_renders_capped_tool_result_body_with_recovery_hint() {
        let big_body: String = (1..=15)
            .map(|i| format!("output-line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = capture(RenderMode::Verbose, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("b1"),
                tool: "Bash".into(),
                params: json!({"command": "echo hi"}),
                parent_tool_call_id: None,
            })
            .expect("render call");
            r.render_event(&AgentEvent::ToolResult {
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("b1"),
                output: big_body,
                is_error: false,
            })
            .expect("render result");
        });
        assert!(out.contains("output-line-1"), "{out:?}");
        assert!(out.contains("output-line-10"), "{out:?}");
        assert!(
            !out.contains("output-line-11"),
            "body cap should drop line 11: {out:?}",
        );
        assert!(out.contains("more lines"), "missing recovery hint: {out:?}",);
        assert!(
            out.contains("loom logs -b wx-1 --tool b1"),
            "recovery hint must reference the bead and tool call id: {out:?}",
        );
    }

    /// Default mode does NOT render `ToolResult` bodies — only the
    /// `ToolCall` summary line. Pin this so verbose-only behavior
    /// doesn't leak into the default render path.
    #[test]
    fn default_mode_suppresses_tool_result_body() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("b1"),
                tool: "Bash".into(),
                params: json!({"command": "echo hi"}),
                parent_tool_call_id: None,
            })
            .expect("render call");
            r.render_event(&AgentEvent::ToolResult {
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("b1"),
                output: "result body".into(),
                is_error: false,
            })
            .expect("render result");
        });
        assert!(out.contains("Bash"), "{out:?}");
        assert!(!out.contains("result body"), "{out:?}");
    }

    /// R4 — Renderer wraps `ToolCall` summary cells in OSC 8 hyperlinks
    /// when the OSC 8 context is enabled. Cmd-click on `src/lib.rs` in
    /// the rendered line opens the file in the user's editor.
    #[test]
    fn tool_call_line_wraps_paths_in_osc8_when_enabled() {
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
            RenderMode::Default,
            BeadId::new("wx-1").expect("valid bead id"),
            false,
            false,
        )
        .with_osc8(tool_body::Osc8Context::enabled(std::path::PathBuf::from(
            "/workspace",
        )));
        r.render_event(&AgentEvent::ToolCall {
            envelope: EventEnvelope::default(),
            id: ToolCallId::new("e1"),
            tool: "Edit".into(),
            params: json!({
                "file_path": "src/lib.rs",
                "old_string": "old\n",
                "new_string": "new\n",
            }),
            parent_tool_call_id: None,
        })
        .expect("render");
        let out = String::from_utf8(cell.lock().expect("not poisoned").clone()).expect("utf-8");
        assert!(
            out.contains("\x1b]8;;file:///workspace/src/lib.rs"),
            "Edit summary cell must wrap the path in OSC 8: {out:?}",
        );
        assert!(
            out.contains("src/lib.rs"),
            "display text still present: {out:?}"
        );
    }

    /// R4 — Without OSC 8 (default), the rendered line contains plain
    /// text with no escape bytes around the path. Silent degradation.
    #[test]
    fn tool_call_line_no_osc8_by_default() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("e1"),
                tool: "Edit".into(),
                params: json!({
                    "file_path": "src/lib.rs",
                    "old_string": "old\n",
                    "new_string": "new\n",
                }),
                parent_tool_call_id: None,
            })
            .expect("render");
        });
        assert!(out.contains("src/lib.rs"), "{out:?}");
        assert!(
            !out.contains("\x1b]8;;"),
            "default context must not emit OSC 8 escapes: {out:?}",
        );
    }

    /// Verbose mode marks tool-error results with a `[tool error]` line
    /// after the body. Used by the renderer-driven failure UI.
    #[test]
    fn verbose_mode_flags_tool_errors_after_body() {
        let out = capture(RenderMode::Verbose, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("b1"),
                tool: "Bash".into(),
                params: json!({"command": "false"}),
                parent_tool_call_id: None,
            })
            .expect("render call");
            r.render_event(&AgentEvent::ToolResult {
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("b1"),
                output: "exit 1".into(),
                is_error: true,
            })
            .expect("render result");
        });
        assert!(out.contains("[tool error]"), "{out:?}");
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
            let r = build_renderer(mode, Box::new(writer), bead.clone(), false, true);
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

    /// H6 — when a `ToolCall` carries `parent_tool_call_id = Some(parent)`,
    /// the renderer prints it indented by 2 spaces beyond the parent.
    /// Tracks indent depth per tool_call_id so nested chains layer
    /// correctly (Task → Read → Edit etc.).
    #[test]
    fn task_subagent_nesting_indents_nested_tool_calls() {
        let out = capture(RenderMode::Default, false, false, |r| {
            // Top-level Task call — no parent, depth 0.
            r.render_event(&AgentEvent::ToolCall {
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("task1"),
                tool: "Task".into(),
                params: json!({}),
                parent_tool_call_id: None,
            })
            .expect("render task");
            // Nested call inside the Task — depth 1, prefixed with two
            // extra spaces.
            r.render_event(&AgentEvent::ToolCall {
                envelope: EventEnvelope::default(),
                id: ToolCallId::new("read1"),
                tool: "Read".into(),
                params: json!({"file_path": "a"}),
                parent_tool_call_id: Some(ToolCallId::new("task1")),
            })
            .expect("render nested");
        });
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 lines, got: {out:?}");
        // Top-level: 2-space prefix (the renderer's base indent).
        assert!(lines[0].starts_with("  Task"), "{}", lines[0]);
        // Nested: base 2 + indent 2 = 4 spaces before the tool name.
        assert!(lines[1].starts_with("    Read"), "{}", lines[1]);
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

    /// Build a `ToolCall` + `ToolResult` pair with explicit `ts_ms`
    /// values so the test can pin pair duration without touching
    /// either the wall clock or the renderer's `Clock` injection.
    fn tool_pair_with_ts(
        id: &str,
        tool: &str,
        params: serde_json::Value,
        call_ts: i64,
        result_ts: i64,
    ) -> (AgentEvent, AgentEvent) {
        let call_env = EventEnvelope {
            ts_ms: call_ts,
            ..EventEnvelope::default()
        };
        let result_env = EventEnvelope {
            ts_ms: result_ts,
            ..EventEnvelope::default()
        };
        (
            AgentEvent::ToolCall {
                envelope: call_env,
                id: ToolCallId::new(id),
                tool: tool.to_string(),
                params,
                parent_tool_call_id: None,
            },
            AgentEvent::ToolResult {
                envelope: result_env,
                id: ToolCallId::new(id),
                output: String::new(),
                is_error: false,
            },
        )
    }

    /// H2 + spec criterion `test_tool_call_result_pairing` — a paired
    /// `tool_call` + `tool_result` renders as a single closing line
    /// whose `Ns` duration comes from the events' `ts_ms` delta. We
    /// use the live + indicator path (color=true) because that is the
    /// branch where pair collapse is visible — the in-place line is
    /// rewritten with the final glyph + duration.
    #[test]
    fn tool_call_result_pairing_collapses_with_ts_ms_duration() {
        // color=true → indicator on (live + non-parallel). Pair the
        // result with a 3.5s ts_ms delta so the test can pin the
        // rendered duration independent of wall clock.
        let out = capture(RenderMode::Default, false, true, |r| {
            let (call, result) = tool_pair_with_ts(
                "t1",
                "Bash",
                serde_json::json!({"command": "cargo test"}),
                1_000,
                4_500,
            );
            r.render_event(&call).expect("render call");
            r.render_event(&result).expect("render result");
        });
        // After ToolResult, the running line is finalized — `\r` +
        // clear-to-EOL + the closing summary with the pair duration.
        assert!(out.contains("Bash"), "{out:?}");
        assert!(out.contains("cargo test"), "{out:?}");
        // ts_ms delta = 3500ms → "3.5s".
        assert!(out.contains("3.5s"), "expected pair duration 3.5s: {out:?}");
        // Single combined block: only one trailing newline after the
        // final summary, i.e. no separate ToolResult line.
        let trailing_newlines = out.chars().rev().take_while(|c| *c == '\n').count();
        assert_eq!(trailing_newlines, 1, "{out:?}");
    }

    /// H2 + spec criterion `test_live_vs_replay_distinction` — the
    /// `PrettyRenderer` (and `TerminalRenderer`) take a `live: bool`
    /// switch. Live emits the in-place `running...` indicator while a
    /// tool is in flight; replay suppresses it entirely and computes
    /// the pair's duration from `ts_ms` deltas.
    #[test]
    fn live_vs_replay_distinction_pretty_renderer() {
        // Live path: indicator appears between ToolCall and ToolResult.
        let (live_buf, live_writer) = captured();
        let mut live_pretty =
            PrettyRenderer::new(live_writer, BeadId::new("wx-1").expect("id"), false, true);
        let (call, result) = tool_pair_with_ts(
            "t1",
            "Bash",
            serde_json::json!({"command": "sleep 3"}),
            0,
            3_000,
        );
        live_pretty.render_event(&call).expect("live call");
        // Snapshot mid-pair: the running line is present.
        let mid = captured_str(&live_buf);
        assert!(
            mid.contains("running..."),
            "live mode must emit in-place running indicator: {mid:?}",
        );
        live_pretty.render_event(&result).expect("live result");
        let live_final = captured_str(&live_buf);
        assert!(live_final.contains("3.0s"), "{live_final:?}");

        // Replay path: indicator is suppressed; the ToolCall summary
        // prints once and the ToolResult appends a closing line under
        // it with the ts_ms-derived duration.
        let (replay_buf, replay_writer) = captured();
        let mut replay_pretty = PrettyRenderer::new(
            replay_writer,
            BeadId::new("wx-1").expect("id"),
            false,
            false,
        );
        replay_pretty.render_event(&call).expect("replay call");
        let mid_replay = captured_str(&replay_buf);
        assert!(
            !mid_replay.contains("running..."),
            "replay must NOT emit the running indicator: {mid_replay:?}",
        );
        replay_pretty.render_event(&result).expect("replay result");
        let replay_final = captured_str(&replay_buf);
        assert!(
            replay_final.contains("3.0s"),
            "replay duration from ts_ms delta: {replay_final:?}",
        );
    }

    /// Spec criterion `test_logs_reuses_renderer` — the `Renderer`
    /// trait + impls are reused between `loom run` and `loom logs`. We
    /// pin this by deserializing events from a JSONL line (the on-disk
    /// shape `loom logs` reads) and feeding them through the same
    /// `build_renderer` path `loom run` uses. The renderer trait
    /// object is the only contract both share.
    #[test]
    fn logs_reuses_renderer_via_jsonl_round_trip() {
        let bead = BeadId::new("wx-1").expect("id");
        let (call, result) = tool_pair_with_ts(
            "t1",
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            0,
            2_000,
        );
        // Round-trip through JSONL — same path `loom logs` would take
        // when replaying a saved log file.
        let lines = [
            serde_json::to_string(&call).expect("ser call"),
            serde_json::to_string(&result).expect("ser result"),
        ];
        let replayed: Vec<AgentEvent> = lines
            .iter()
            .map(|s| serde_json::from_str::<AgentEvent>(s).expect("deser"))
            .collect();

        // Build via `build_renderer` (the public selection function
        // shared by `loom run` and `loom logs`) with `live=false`.
        let (buf, writer) = captured();
        let mut r = build_renderer(RenderMode::Plain, Box::new(writer), bead, false, false);
        for ev in &replayed {
            r.render_event(ev).expect("render");
        }
        r.finish(BeadOutcome::Done, Duration::from_secs(2))
            .expect("finish");
        let out = captured_str(&buf);
        assert!(out.contains("Bash"), "{out:?}");
        // Replay path emits a closing glyph + ts_ms-derived duration
        // for the pair.
        assert!(out.contains("2.0s"), "{out:?}");
    }
}
