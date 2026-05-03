# Loom Agent

Agent backend abstraction, pi-mono and Claude Code implementations, container
communication, and agent runtime layer for the pi runtime.

## Problem Statement

Ralph's bash scripts launch Claude Code as the only agent runtime, creating
vendor lock-in to Anthropic. Users with a Claude Max subscription need the
`claude` binary; users who want LLM-agnostic switching need an alternative.
Pi-mono provides 20+ LLM provider backends and an NDJSON RPC mode that enables
programmatic control — but it requires a different communication protocol than
Claude Code's stream-json output mode.

As of April 2026, Anthropic no longer allows third-party applications to consume
Claude Pro/Max subscription quota. This means pi-mono cannot use a Max
subscription even when backed by Claude — validating the need for a dedicated
Claude Code backend that runs the `claude` binary directly.

This spec defines the agent abstraction that lets Loom drive either runtime
through a common interface, and the infrastructure changes (runtime layer,
entrypoint) that make pi-mono available inside wrapix containers. The Loom
platform (crate structure, templates, workflow) is defined in
[loom-harness.md](loom-harness.md).

## Requirements

### Functional

1. **Host-side execution** — Loom runs on the host, not inside containers. It
   spawns per-bead containers by invoking `wrapix run-bead --spawn-config
   <file> --stdio` (a thin wrapix subcommand that owns container construction)
   and communicates with the agent process inside via stdin/stdout pipes.
   Loom never calls `podman run` directly; see
   [loom-harness.md — Process Architecture](loom-harness.md#process-architecture).
2. **Agent backend trait** — an async Rust trait (`AgentBackend`) abstracting
   agent lifecycle: spawn a session and declare capabilities. Used via type
   parameter (`<B: AgentBackend>`) — the concrete backend is known at each
   call site.
3. **Pi backend** — speaks pi-mono's NDJSON RPC protocol over stdin/stdout.
   Supports:
   - `prompt` — send initial or follow-up prompts
   - `steer` — mid-session course correction
   - `abort` — terminate current operation
   - `set_thinking_level` — adjust reasoning effort
   - `set_model` — switch LLM provider/model mid-session
   - Streaming event parsing (message deltas, tool calls, tool results,
     completion, compaction, errors)
4. **Claude backend** — launches
   `claude --print --input-format stream-json --output-format stream-json`,
   parses NDJSON events from stdout, and writes user messages (initial
   prompt, steering) as stream-json on stdin. `--permission-prompt-tool
   stdio` enables the `control_request` / `control_response` flow. The
   `--print` flag keeps the session non-interactive (runs to completion,
   exits) while `--input-format stream-json` enables mid-session steering
   via additional user messages. On observing a `result` event, loom closes
   its end of stdin, waits `[claude] post_result_grace_secs` (default 5s)
   for natural exit, then escalates SIGTERM → SIGKILL — same pattern as
   Ralph's `run_claude_stream` watchdog (`lib/ralph/cmd/util.sh`).
5. **Per-phase backend selection** — each workflow phase (plan, todo, run,
   check, msg) independently resolves its backend and model from config.
   The `default` key under `[agent]` sets the fallback (`claude`). Per-phase
   overrides (e.g. `[agent.todo]`) specify backend + provider + model.
   `--agent` CLI flag overrides all phase config for the current invocation.
6. **Agent runtime layer** — the image builder composes two orthogonal axes:
   *workspace profile* (base, rust, python) and *agent runtime* (claude, pi).
   When `WRAPIX_AGENT=pi`, the pi runtime layer (Node.js + pi binary) is
   added to whichever workspace profile the bead requires. No standalone
   `profiles.pi` — no profile proliferation.
7. **Entrypoint agent selection** — `entrypoint.sh` checks `WRAPIX_AGENT` and:
   - `claude` (default): existing behavior (Claude config merging, hooks,
     `claude --dangerously-skip-permissions`)
   - `pi`: skips Claude-specific config, starts `pi --mode rpc` listening on
     stdin/stdout
8. **Event normalization** — both backends emit a common `AgentEvent` enum so
   the workflow engine does not need backend-specific event handling.
9. **NDJSON framing** — both protocols use Newline-Delimited JSON (one complete
   JSON object per line, separated by `\n`). The NDJSON reader splits on `\n`
   only, not Unicode line separators (U+2028, U+2029). Each line is
   independently parseable.

### Non-Functional

1. **No podman socket mounting** — `wrapix run-bead` invokes podman on the
   host; the agent runs inside the resulting container with no access to the
   podman socket. No nested container support needed.
2. **Graceful degradation** — if a backend-specific feature is unavailable
   (e.g. pi providers that don't support `set_thinking_level`, or pi
   builds where manual `compact` is disabled), the driver continues
   without it. No hard failures for missing optional capabilities.
3. **Parse, Don't Validate** — raw protocol bytes are parsed into typed domain
   representations at the NDJSON boundary. All code downstream of the parser
   works with already-validated types. No re-parsing, no stringly-typed event
   matching.
4. **Static dispatch** — `AgentBackend` uses an explicit type parameter
   (`<B: AgentBackend>`), not a trait object. Backends are zero-sized types
   with associated functions (no `&self`). A `dispatch` function in the
   binary crate matches on `AgentKind` per phase and calls
   `run_agent::<ConcreteType>`. No `async-trait` needed — `async fn` in
   traits is stable and works directly with static dispatch.

## Architecture

### Dispatch: ZST Backends + Per-Phase Selection

Backends are zero-sized types — all runtime state lives in `AgentSession` and
`SpawnConfig`. The type parameter alone carries the dispatch:

```rust
pub struct PiBackend;
pub struct ClaudeBackend;
```

No instances, no `new()`. Trait methods are associated functions (no `&self`).

The backend is resolved **per phase** from config, not once at startup.
Each workflow command (plan, todo, run, check, msg) independently selects
its backend + model. A `dispatch` function in the binary crate closes over
the concrete types:

```rust
// main.rs — the only place that knows PiBackend and ClaudeBackend
async fn dispatch(
    phase: Phase,
    config: &LoomConfig,
    spawn: &SpawnConfig,
) -> Result<SessionOutcome, ProtocolError> {
    match config.agent_for(phase) {
        AgentKind::Pi => run_agent::<PiBackend>(spawn).await,
        AgentKind::Claude => run_agent::<ClaudeBackend>(spawn).await,
    }
}
```

The workflow engine receives `dispatch` as a parameter — it never touches
concrete backend types. Static dispatch is preserved inside each match arm.
The compiler monomorphizes two copies of `run_agent`.

**Per-phase config example:** `loom todo` uses a cheap model via pi, while
`loom check` uses claude directly:

```toml
[agent]
default = "claude"

[agent.todo]
backend = "pi"
provider = "deepseek"
model_id = "deepseek-v3"

[agent.check]
backend = "claude"
```

Phases without explicit config inherit `[agent] default`. The pi backend
calls `set_model` after spawn if the phase config specifies a provider/model.

**For testing:** mock backends are ZSTs too:
`run_agent::<MockBackend>(&config)`.

```rust
// loom-workflow (simplified — real version handles retry, steering, logging)
pub async fn run_agent<B: AgentBackend>(
    config: &SpawnConfig,
) -> Result<SessionOutcome, ProtocolError> {
    let session = B::spawn(config).await?;
    let mut session = session.prompt(&config.initial_prompt).await?;
    loop {
        match session.next_event().await? {
            Some(AgentEvent::SessionComplete { exit_code, cost_usd }) => {
                return Ok(SessionOutcome { exit_code, cost_usd });
            }
            Some(event) => {
                tracing::debug!(?event, "agent event");
            }
            None => {
                return Err(ProtocolError::UnexpectedEof);
            }
        }
    }
}
```

### Agent Backend Trait

```rust
// loom-core

pub trait AgentBackend: Send + Sync {
    async fn spawn(
        config: &SpawnConfig,
    ) -> Result<AgentSession<Idle>, ProtocolError>;
}
```

The trait is deliberately minimal — it only handles process lifecycle. Session
interaction (prompt, steer, abort, event streaming) is on the typestate
`AgentSession`, not the backend trait. The backend's job is to spawn a session;
the session's job is to drive the conversation.

Both backends support steering: pi via the native `steer` command, claude
via `--input-format stream-json --output-format stream-json` (sends a
stream-json user message on stdin during the session). No
`SUPPORTS_STEERING` capability gate — `AgentSession::steer` works for both
backends. If a future backend cannot support steering, reintroduce the
constant.

No `&self` — backends are ZSTs, so the type parameter carries all
information. No `#[async_trait]` — `async fn` in traits is native with
edition 2024 and static dispatch.

### Typestate Session

Invalid protocol transitions are compile errors. An `AgentSession<Idle>` must be
prompted before events can be read. An `AgentSession<Active>` cannot be prompted
again — it must complete or be aborted first.

```rust
pub struct Idle;
pub struct Active;

pub struct AgentSession<S> {
    child: tokio::process::Child,
    stdin: BufWriter<ChildStdin>,
    reader: NdjsonReader,
    parser: Box<dyn LineParse>,
    pending: VecDeque<AgentEvent>,
    _state: PhantomData<S>,
}

impl AgentSession<Idle> {
    pub async fn prompt(
        self,
        msg: &str,
    ) -> Result<AgentSession<Active>, ProtocolError> {
        // Write prompt as NDJSON command to stdin, transition to Active
    }
}

impl AgentSession<Active> {
    pub async fn next_event(
        &mut self,
    ) -> Result<Option<AgentEvent>, ProtocolError> {
        // Drain self.pending first. Then read next NDJSON line, parse via
        // LineParse. If ParsedLine::response is Some, write it to stdin.
        // Push excess events into self.pending, return the first one.
    }

    pub async fn steer(
        &mut self,
        msg: &str,
    ) -> Result<(), ProtocolError> {
        // Pi: NDJSON steer command. Claude: stream-json user message.
    }

    pub async fn abort(
        self,
    ) -> Result<AgentSession<Idle>, ProtocolError> {
        // Pi: send abort command, return to Idle (session reusable).
        // Claude: kill process, return to Idle (session is dead — prompt
        //   will fail with ProcessExit).
    }
}
```

`AgentSession`, `NdjsonReader`, and `LineParse` all live in loom-core (not
loom-agent) because the `AgentBackend` trait returns `AgentSession<Idle>` —
if these types lived in loom-agent, the trait would depend on its own
implementor crate (circular dependency).

```rust
// loom-core

pub struct ParsedLine {
    pub events: Vec<AgentEvent>,
    pub response: Option<String>,
}

pub trait LineParse: Send {
    fn parse_line(&self, line: &str) -> Result<ParsedLine, ProtocolError>;
}
```

Each backend (in loom-agent) provides its own `LineParse` implementation:
`PiParser` and `ClaudeParser`. `Box<dyn LineParse>` inside the session keeps
`AgentSession` a single concrete type that both backends share. The per-line
vtable call is negligible next to the IO cost of reading from a subprocess
pipe. Static dispatch on `AgentBackend` (the outer layer) eliminates the
`async-trait` dependency; internal dyn on `LineParse` (the inner layer) avoids
leaking backend types through the session's public API.
`ParsedLine::response` handles protocol control flow: when a parsed line
requires a response on stdin (e.g., Claude's `control_request` auto-approve),
the parser populates this field. `AgentSession::next_event` writes the
response before yielding events — keeping response logic in the parser and IO
in the session. `ParsedLine::events` is a `Vec` because some protocol messages map to
multiple `AgentEvent`s: Claude's `result/success` produces `TurnEnd` +
`SessionComplete`; `result/error` produces `Error` + `SessionComplete`.
Pi's `turn_end` and `agent_end` are separate events that each map to a
single `AgentEvent`.

### AgentEvent

```rust
#[derive(Debug)]
pub enum AgentEvent {
    /// Streaming text fragment from the agent.
    MessageDelta { text: String },

    /// Agent invoked a tool.
    ToolCall {
        id: ToolCallId,
        tool: String,
        params: serde_json::Value,
    },

    /// Tool execution completed.
    ToolResult {
        id: ToolCallId,
        output: String,
        is_error: bool,
    },

    /// Agent finished one turn (may have more turns in a multi-turn session).
    TurnEnd,

    /// Agent session completed — process exiting or final result received.
    SessionComplete {
        exit_code: i32,
        cost_usd: Option<f64>,
    },

    /// Agent context was compacted.
    CompactionStart { reason: CompactionReason },
    CompactionEnd { aborted: bool },

    /// Agent reported an error.
    Error { message: String },
}

#[derive(Debug)]
pub enum CompactionReason {
    ContextLimit,
    UserRequested,
    Unknown,
}
```

### ProtocolError

```rust
#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum ProtocolError {
    /// invalid JSON on protocol line
    InvalidJson(#[from] serde_json::Error),

    /// unknown message type: {0}
    UnknownMessageType(String),

    /// IO error
    Io(#[from] std::io::Error),

    /// agent process exited with code {0}
    ProcessExit(i32),

    /// unexpected end of event stream
    UnexpectedEof,

    /// NDJSON line too long: {len} bytes (max {max})
    LineTooLong { len: usize, max: usize },

    /// operation not supported by this backend
    Unsupported,
}
```

### SpawnConfig

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct SpawnConfig {
    pub image: String,
    pub workspace: PathBuf,
    pub env: Vec<(String, String)>,
    pub initial_prompt: String,
    pub agent_args: Vec<String>,
    pub repin: RePinContent,
}
```

`SpawnConfig` is `Serialize` + `Deserialize` because loom writes it to a
JSON file and `wrapix run-bead --spawn-config <file>` reads it back. This is
the single serialization boundary between loom and the wrapper — preferred
over a fat argv interface. The wrapper's JSON shape is the stable contract;
loom and `wrapix run-bead` ship from the same flake and stay in lockstep.

`env` is an **explicit allowlist** — only listed variables are forwarded
into the container by `wrapix run-bead` (which materializes them as
`podman run -e`). The host environment is never inherited wholesale. The
workflow engine constructs this list from known-needed variables:

| Variable | When | Purpose |
|----------|------|---------|
| `WRAPIX_AGENT` | always | Agent selection in entrypoint |
| `CLAUDE_CODE_OAUTH_TOKEN` | claude backend | Claude authentication |
| `ANTHROPIC_API_KEY` | pi backend (Anthropic models) | LLM API key |
| `TERM` | always | Terminal capability |
| `BEADS_DOLT_SERVER_SOCKET`, `BEADS_DOLT_AUTO_START` | always | Beads dolt-socket path (set to the bind-mount); auto-start disabled (host owns the server) |

Provider-specific API keys for the pi backend (OpenAI, Google, etc.) are
added only when the configured model requires them. Variable names are
logged at `info!` level during spawn; values are never logged.

```rust
#[derive(Debug)]
pub struct SessionOutcome {
    pub exit_code: i32,
    pub cost_usd: Option<f64>,
}
```

### Host-to-Container Communication

```
loom (host)                                            container
    │                                                       │
    ├─ serialize SpawnConfig → /tmp/loom-<id>.json          │
    ├─ wrapix run-bead --spawn-config <file> --stdio        │
    │   └─ exec podman run [no TTY, stdio piped] ─►  entrypoint.sh
    │                                                       │
    │                                                  agent (pi --mode rpc / claude)
    │   stdin ──────────────────────────────────────►  agent
    │   stdout ◄──────────────────────────────────────  agent
    │                                                       │
    ├─ writes NDJSON to stdin ────────────────────────►  agent processes command
    │   (both backends)                                     │
    │                                                       │
    ├─ reads NDJSON from stdout ◄─────────────────────  agent streams events
    │   (both backends)                                     │
    │                                                       │
    └─ on exit: container teardown via wrapix              │
```

The wrapper hides container construction (mounts, env allowlist, krun
runtime, network filter, deploy key, beads dolt socket) so loom owns only
NDJSON framing and the typed `SpawnConfig` it serializes. Both backends use
bidirectional NDJSON over stdin/stdout. The Claude backend uses
`--input-format stream-json --output-format stream-json` for full
bidirectional support.

### NDJSON Framing

Newline-Delimited JSON (NDJSON/JSONL): each line is one complete JSON object,
terminated by `\n` (0x0A). Both pi RPC and Claude stream-json use this framing.

Parsing rules:

- Split on `\n` (0x0A). Trailing `\r` is stripped.
- U+2028 and U+2029 are NOT line terminators — they pass through as JSON content.
- Empty lines (blank between objects) are silently skipped.
- Each non-empty line is independently parsed as JSON.
- A line that fails JSON parsing is a `ProtocolError::InvalidJson`.

```rust
const MAX_LINE_BYTES: usize = 10 * 1024 * 1024; // 10 MB

pub struct NdjsonReader {
    reader: BufReader<ChildStdout>,
    line_buf: String,
}

impl NdjsonReader {
    pub async fn next_line(
        &mut self,
    ) -> Result<Option<&str>, ProtocolError> {
        loop {
            self.line_buf.clear();
            let n = self.reader.read_line(&mut self.line_buf).await?;
            if n == 0 {
                return Ok(None);
            }
            if self.line_buf.len() > MAX_LINE_BYTES {
                return Err(ProtocolError::LineTooLong {
                    len: self.line_buf.len(),
                    max: MAX_LINE_BYTES,
                });
            }
            let trimmed = self.line_buf.trim_end_matches(['\n', '\r']);
            if !trimmed.is_empty() {
                return Ok(Some(trimmed));
            }
        }
    }
}
```

`MAX_LINE_BYTES` prevents a malicious or malfunctioning agent from exhausting
host memory by sending a single line without a `\n` terminator. 10 MB is well
above any legitimate NDJSON message (the largest are tool results with file
contents). The limit is checked after the read completes — `read_line` will
still buffer the full line, but the error fires before the line is parsed or
accumulated further.

### Pi-Mono RPC Protocol

Pi's `--mode rpc` uses NDJSON over stdin/stdout. The protocol has no version
negotiation or handshake. After spawning the process, the Pi backend sends a
`get_commands` probe and verifies the response contains every command Loom
depends on. The probe set is the union of commands actually used by the
backend: at minimum `prompt`, `steer`, `abort`, `set_model`, plus `compact`
(if manual compaction is wired up) and `get_session_stats` (if cost capture
is enabled — see [Pi cost tracking](#pi-cost-tracking)). If the response
shape is unexpected or a required command is missing, the backend fails fast
with a clear version-mismatch error before any workflow begins. After the
probe succeeds, normal command flow starts.

Messages are classified by a two-phase deserialization strategy: peek at the
`type` and `id` fields to determine the message category, then deserialize
the full payload into the correct type.

**Two-phase deserialization:**

```rust
#[derive(Debug, Deserialize)]
struct PiEnvelope {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    id: Option<RequestId>,
}

#[derive(Debug)]
pub enum PiMessage {
    Response(PiResponse),
    Event(PiEvent),
    ExtensionUiRequest(PiUiRequest),
}

pub fn parse_pi_line(line: &str) -> Result<PiMessage, ProtocolError> {
    let env: PiEnvelope = serde_json::from_str(line)?;
    match env.msg_type.as_deref() {
        Some("response") => {
            let resp: PiResponse = serde_json::from_str(line)?;
            Ok(PiMessage::Response(resp))
        }
        Some("extension_ui_request") => {
            let req: PiUiRequest = serde_json::from_str(line)?;
            Ok(PiMessage::ExtensionUiRequest(req))
        }
        _ if env.id.is_none() => {
            let evt: PiEvent = serde_json::from_str(line)?;
            Ok(PiMessage::Event(evt))
        }
        _ => Err(ProtocolError::UnknownMessageType(
            env.msg_type.unwrap_or_default(),
        )),
    }
}
```

**Why two-phase?** Pi messages don't follow a clean tagged union: responses have
`type: "response"` plus an `id`, events carry their own `type` values (e.g.,
`"message_update"`) without an `id`, and extension UI requests have
`type: "extension_ui_request"`. The discriminant logic is `(type, id)` — a
two-field dispatch that serde's built-in tag/content support can't express.

The envelope parse is cheap (serde skips unknown fields), and the second parse
deserializes into the exact target type.

**Response envelope:**

```rust
#[derive(Debug, Deserialize)]
pub struct PiResponse {
    pub id: RequestId,
    pub command: String,
    pub success: bool,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}
```

The `command` field echoes back the command name. The `success` boolean
discriminates between a successful result (payload in `data`) and a failure
(message in `error`). The driver checks `success` before accessing `data`.

**Commands (driver → pi, via stdin):**

All commands are NDJSON objects with a `type` field. Every command supports an
optional `id: String` field for request-response correlation — if provided, the
response echoes it back.

| Command | Fields | Purpose |
|---------|--------|---------|
| `prompt` | `message`, `images?`, `streamingBehavior?` | Send prompt. `streamingBehavior`: `"steer"` or `"followUp"` — controls queuing of messages sent during streaming |
| `steer` | `message`, `images?` | Mid-session course correction (queued during streaming) |
| `follow_up` | `message`, `images?` | Follow-up after turn completion |
| `abort` | — | Terminate current operation |
| `set_model` | `provider`, `modelId` | Switch LLM provider and model (two separate fields) |
| `set_thinking_level` | `level` | Adjust reasoning: `off`, `minimal`, `low`, `medium`, `high`, `xhigh` |
| `new_session` | `parentSession?` | Start fresh session (optional parent for forking) |
| `compact` | `customInstructions?` | Trigger manual compaction |
| `set_auto_compaction` | `enabled` | Toggle automatic compaction |
| `get_commands` | — | Probe available commands (startup validation) |

Loom uses the commands above. Pi supports additional commands that Loom does not
use in v1: `get_state`, `get_messages`, `get_session_stats`, `cycle_model`,
`get_available_models`, `cycle_thinking_level`, `set_steering_mode`,
`set_follow_up_mode`, `set_auto_retry`, `abort_retry`, `bash`, `abort_bash`,
`export_html`, `switch_session`, `fork`, `clone`, `get_fork_messages`,
`get_last_assistant_text`, `set_session_name`.

**Events (pi → driver, via stdout):**

Pi events have a `type` field and no `id`. The `message_update` event contains
a nested `assistantMessageEvent` with its own delta types — the parser must
dispatch on both levels.

| Event | Key Fields | Maps To |
|-------|------------|---------|
| `message_update` | `assistantMessageEvent` | see delta mapping below |
| `tool_execution_start` | `toolCallId`, `toolName`, `args` | `AgentEvent::ToolCall` |
| `tool_execution_end` | `toolCallId`, `toolName`, `result`, `isError` | `AgentEvent::ToolResult` |
| `tool_execution_update` | `toolCallId`, `partialResult` | logged at `trace!`, skipped |
| `turn_start` | — | logged at `trace!`, skipped |
| `turn_end` | `message`, `toolResults` | `AgentEvent::TurnEnd` |
| `agent_start` | — | logged at `trace!`, skipped |
| `agent_end` | `messages` | `AgentEvent::SessionComplete` (`exit_code: 0`) — see note below |
| `compaction_start` | `reason` | `AgentEvent::CompactionStart` |
| `compaction_end` | `aborted`, `reason`, `result?`, `willRetry`, `errorMessage?` | `AgentEvent::CompactionEnd` |
| `queue_update` | `steering`, `followUp` | logged at `trace!`, skipped |
| `auto_retry_start` | `attempt`, `maxAttempts`, `delayMs`, `errorMessage` | logged at `debug!`, skipped |
| `auto_retry_end` | `success`, `attempt`, `finalError` | logged at `debug!`, skipped |
| `extension_error` | `extensionPath`, `event`, `error` | logged at `debug!`, skipped |

**Compaction reasons:** Pi uses `"threshold"` (approaching limit) and
`"overflow"` (already exceeded) — both map to `CompactionReason::ContextLimit`.
`"manual"` (user-triggered) maps to `CompactionReason::UserRequested`. These
are the only reasons emitted by pi as of v0.72.

**`agent_end` semantics:** In pi, `agent_end` signals "this prompt cycle is
done" — the process keeps accepting commands. Loom's per-bead-container
model maps this to `SessionComplete` because each container handles exactly
one prompt; after `agent_end`, loom tears down the container rather than
sending another command. If the per-bead model ever changes, this mapping
must be revisited.

**`message_update` delta mapping:**

The `assistantMessageEvent` sub-object carries a delta `type` field. Most
deltas are observability-only — tool lifecycle and turn boundaries are handled
by the top-level `tool_execution_*` and `turn_end` events.

| Delta Type | Maps To |
|------------|---------|
| `text_delta` | `AgentEvent::MessageDelta` (extract `text`) |
| `error` | `AgentEvent::Error` (reasons: `"aborted"`, `"error"`) |
| `start`, `text_start`, `text_end` | logged at `trace!`, skipped |
| `thinking_start`, `thinking_delta`, `thinking_end` | logged at `trace!`, skipped |
| `toolcall_start`, `toolcall_delta`, `toolcall_end` | logged at `trace!`, skipped |
| `done` | logged at `trace!`, skipped (reasons: `"stop"`, `"length"`, `"toolUse"`) |

**Extension UI passthrough:** Pi emits `extension_ui_request` messages for
extension-defined UI. Loom logs these at `debug!` level — no pi extensions
are loaded in the wrapix sandbox, so this should not arise in practice.
However, the timeout on these requests is set by the *extension*, not
enforced by pi: if an extension does not specify `timeout?` and the host
does not respond, the extension's promise hangs forever and may stall the
agent. As a defensive fallback, when loom observes an
`extension_ui_request` whose `method` requires a response
(`select`/`confirm`/`input`/`editor`), it replies with
`{"type":"extension_ui_response","id":"<request_id>","cancelled":true}`.
Methods that don't need a response (`notify`/`setStatus`/`setWidget`/
`setTitle`/`set_editor_text`) are logged and ignored.

**Stdout discipline:** Pi v0.72+ calls `takeOverStdout()` at RPC startup —
`process.stdout.write` is monkey-patched to redirect non-protocol writes to
stderr, while protocol output uses a captured raw fd via `writeRawStdout`
(see `output-guard.ts`). Extensions, libraries, and OSC escape sequences
cannot corrupt the protocol stream. The earlier corruption issue
([pi-mono#2388](https://github.com/badlogic/pi-mono/issues/2388)) was fixed
in March 2026. The NDJSON parser's malformed-line handling (log warning,
skip line) is retained as defensive coding, not a live mitigation.

### Claude Stream-JSON Protocol

Claude Code's `--output-format stream-json` emits NDJSON events.
Combined with `--input-format stream-json`, communication is bidirectional.

**Events (claude → driver, via stdout):**

```rust
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeMessage {
    #[serde(rename = "system")]
    System {
        subtype: String,
        session_id: Option<SessionId>,
    },

    #[serde(rename = "assistant")]
    Assistant { message: AssistantContent },

    #[serde(rename = "user")]
    User { message: UserContent },

    #[serde(rename = "result")]
    Result {
        subtype: String,
        result: Option<String>,
        total_cost_usd: Option<f64>,
        duration_ms: Option<u64>,
        num_turns: Option<u32>,
        is_error: Option<bool>,
    },

    #[serde(rename = "control_request")]
    ControlRequest {
        id: RequestId,
        tool: String,
        input: serde_json::Value,
    },

    #[serde(other)]
    Unknown,
}
```

**Why `#[serde(tag = "type")]` works here:** Unlike pi, Claude messages follow a
clean tagged union — every message has a `type` field that uniquely identifies
the variant. Serde's internally-tagged enum handles this directly.

**Event mapping:**

| Claude Event | Maps To |
|-------------|---------|
| `system` (subtype `init`) | session metadata — extract `session_id` |
| `assistant` (tool_use content) | `AgentEvent::ToolCall` |
| `assistant` (text content) | `AgentEvent::MessageDelta` |
| `user` (tool_result content) | `AgentEvent::ToolResult` |
| `result` (subtype `success`) | `AgentEvent::TurnEnd` then `AgentEvent::SessionComplete` |
| `result` (subtype `error`) | `AgentEvent::Error` then `AgentEvent::SessionComplete` |
| `control_request` | log at `info!`, auto-approve via `control_response` on stdin |
| `Unknown` | logged at `debug!`, skipped |

**Permission prompt tool:** With `--permission-prompt-tool stdio`, Claude emits
`control_request` events for tool permissions and expects `control_response` on
stdin. Loom auto-approves tool calls because the container is sandboxed, but
logs every approval at `info!` level with the tool name and a truncated input
summary (first 200 chars). This provides an audit trail and makes unexpected
tool types visible in logs.

```json
{"type": "control_response", "id": "<request_id>", "approved": true}
```

**Deny-list.** A configurable `denied_tools` list in `config.toml` rejects
specific tool names with `approved: false`. Empty by default — the
container sandbox is the trust boundary and logging is the primary
mitigation. The slot exists today so a deny rule can be added without a
loom release if Claude Code ships a tool type that reaches outside the
container boundary.

### Compaction Handling

Both backends use the same `RePinContent` struct (defined in loom-core — see
[loom-harness.md](loom-harness.md#compaction-re-pin)) to restore agent context
after compaction. The content is built once per session by the workflow engine.
**Content is unified; delivery is asymmetric** because Claude stream-json does
not expose compaction events — Anthropic compacts internally with no protocol
notification, so claude must use its own `SessionStart` hook system, while pi
exposes compaction events natively in NDJSON. The asymmetry is fundamental
to the products' compaction models, not a Loom design choice.

**Claude backend:**
- Before spawn, calls `repin.write_claude_files(runtime_dir)` to write
  `repin.sh` + `claude-settings.json` into the container's runtime directory.
- Claude Code's `SessionStart` hook reads these files automatically on each
  compaction. The driver is not involved at compaction time.

**Pi backend:**
- Holds `RePinContent` in memory.
- When a `compaction_start` event arrives in the NDJSON stream, sends
  `repin.to_prompt()` via a `steer` command on stdin.
- **Steer timing.** A `steer` command queues; pi delivers it after the
  current assistant turn finishes its tool calls, before the next LLM call —
  it does not inject content during compaction itself. The re-pin therefore
  reaches the agent on the *next* turn after compaction completes, which is
  the desired effect (post-compact context restoration).
- **Overflow auto-retry.** When `compaction_start.reason == "overflow"` and
  compaction succeeds, pi automatically retries the prompt
  (`compaction_end.willRetry == true`). A steer queued during this window
  interleaves with the auto-retry: it lands on the turn following the
  retry's first response, not before. This is acceptable — the retry plus
  re-pin combined still restore working context — but documented so the
  behavior is not surprising in logs.
- The subsequent `compaction_end` event confirms whether compaction succeeded
  (`aborted: false`) or was abandoned. If pi retries compaction (a fresh
  `compaction_start` arrives), the driver re-pins again.

## Agent Runtime Layer

### Two-Axis Composition

Container images are composed from two independent axes:

| Axis | Options | Determines |
|------|---------|------------|
| **Workspace profile** | base, rust, python | Toolchain packages (cargo, python, etc.) |
| **Agent runtime** | claude, pi | Agent binary and its dependencies |

The image builder (`lib/sandbox/image.nix`) combines the selected profile
with the selected runtime. Examples:

- `profile:rust` + `WRAPIX_AGENT=claude` → rust toolchain + claude binary (current default)
- `profile:rust` + `WRAPIX_AGENT=pi` → rust toolchain + Node.js + pi binary
- `profile:base` + `WRAPIX_AGENT=pi` → base packages + Node.js + pi binary

No standalone `profiles.pi`. No `pi+rust` / `pi+python` proliferation. The
claude runtime layer is empty (claude is already in the base image today).

### Pi Runtime Layer

The pi runtime layer adds:

| Addition | Details |
|----------|---------|
| Node.js | `nodejs_22` (or current LTS) |
| Pi binary | `@mariozechner/pi-coding-agent` built via `buildNpmPackage` |

The layer does NOT include pi's web-ui, development dependencies, or test
infrastructure. Only the runtime binary and its production dependencies.

### Nix Packaging

Pi-mono is a Node.js application distributed via npm:

- Use `buildNpmPackage` to build from source (preferred for reproducibility).
  `npmDepsHash` pins the exact dependency tree — a changed hash fails the
  build, preventing silent supply chain updates.
- Audit `postinstall` scripts before version bumps — npm packages can
  execute arbitrary code at install time. Review the diff of
  `package.json` and any lifecycle scripts.
- Pi-mono runs inside the container, not on the host — a compromised
  runtime's blast radius is limited to the sandbox boundary.
- Alternative: Bun compilation to standalone binary (eliminates Node.js
  dependency but adds Bun to build inputs)
- The `pi` binary is the entry point from `@mariozechner/pi-coding-agent`
- Pin to a specific release tag for stability (pi-mono has daily releases)

### Entrypoint Changes

`lib/sandbox/linux/entrypoint.sh` gains an agent selection branch:

```bash
case "${WRAPIX_AGENT:-claude}" in
  claude)
    # existing behavior: Claude config merge, hooks, launch claude
    ;;
  pi)
    # skip Claude-specific config merging
    # start pi in RPC mode, listening on stdin/stdout
    exec pi --mode rpc
    ;;
esac
```

The pi branch:
- Skips `claude-config.json` and `claude-settings.json` merging
- Skips Claude plugin configuration
- Skips `--dangerously-skip-permissions` (pi has no permission system)
- Preserves: git SSH setup, beads-dolt connection, network filtering,
  session audit logging

## Affected Files

### New

| File | Role |
|------|------|
| `loom/crates/loom-agent/` | Pi and Claude backend implementations (PiBackend, ClaudeBackend, parsers) |

### Modified

| File | Change |
|------|--------|
| `lib/sandbox/image.nix` | Agent runtime layer composition (profile × runtime) |
| `lib/sandbox/linux/entrypoint.sh` | Agent selection via `WRAPIX_AGENT` |
| `modules/flake/overlays.nix` | Pi-mono package overlay |

## Success Criteria

### Agent trait

- [ ] `AgentBackend` trait defined in loom-core with associated `spawn`; no `SUPPORTS_STEERING` constant (both backends steer)
  [verify](tests/loom-test.sh::test_agent_trait_exists)
- [ ] `run_agent` compiles with both `PiBackend` and `ClaudeBackend` as concrete types
  [verify](tests/loom-test.sh::test_agent_trait_static_dispatch)
- [ ] `AgentEvent` enum covers: MessageDelta, ToolCall, ToolResult, TurnEnd, SessionComplete, CompactionStart, CompactionEnd, Error
  [verify](tests/loom-test.sh::test_agent_event_variants)
- [ ] `SpawnConfig` struct captures image, workspace, env, initial_prompt, agent_args, repin
  [verify](tests/loom-test.sh::test_spawn_config_fields)
- [ ] Typestate `AgentSession<Idle>` / `AgentSession<Active>` prevents invalid transitions
  [verify](tests/loom-test.sh::test_typestate_transitions)
- [ ] `ProtocolError` variants cover InvalidJson, UnknownMessageType, Io, ProcessExit, UnexpectedEof, LineTooLong, Unsupported
  [verify](tests/loom-test.sh::test_protocol_error_variants)

### Pi backend

- [ ] Pi backend sends `get_commands` probe on startup and fails fast if required commands are missing
  [verify](tests/loom-test.sh::test_pi_startup_probe)
- [ ] Pi backend parses NDJSON events via two-phase deserialization
  [verify](tests/loom-test.sh::test_pi_two_phase_deser)
- [ ] Pi backend sends NDJSON commands to pi's stdin
  [verify](tests/loom-test.sh::test_pi_rpc_command_sending)
- [ ] Pi backend supports steering (steer returns Ok and reaches the agent on the next turn)
  [verify](tests/loom-test.sh::test_pi_supports_steering)
- [ ] Pi backend maps all pi event types to AgentEvent variants
  [verify](tests/loom-test.sh::test_pi_event_mapping)
- [ ] Pi backend detects CompactionStart event and sends `RePinContent::to_prompt` via steer
  [verify](tests/loom-test.sh::test_pi_compaction_repin)
- [ ] Pi backend handles malformed NDJSON gracefully (logs warning, continues)
  [verify](tests/loom-test.sh::test_pi_malformed_ndjson)
- [ ] Pi backend logs extension_ui_request at debug level without responding
  [verify](tests/loom-test.sh::test_pi_extension_ui_passthrough)

### Claude backend

- [ ] Claude backend parses stream-json NDJSON events from claude's stdout
  [verify](tests/loom-test.sh::test_claude_stream_json_parsing)
- [ ] Claude backend uses `#[serde(tag = "type")]` for tagged enum deserialization
  [verify](tests/loom-test.sh::test_claude_tagged_enum)
- [ ] Claude backend maps claude event types to AgentEvent variants
  [verify](tests/loom-test.sh::test_claude_event_mapping)
- [ ] Claude backend captures cost_usd from result events
  [verify](tests/loom-test.sh::test_claude_cost_capture)
- [ ] Claude backend handles unknown event types via `#[serde(other)]`
  [verify](tests/loom-test.sh::test_claude_unknown_events)
- [ ] Claude backend writes re-pin files via `RePinContent::write_claude_files` before spawn
  [verify](tests/loom-test.sh::test_claude_repin_files)
- [ ] Claude backend auto-approves permission requests via control_response
  [verify](tests/loom-test.sh::test_claude_permission_autoapprove)
- [ ] Claude backend supports steering — sends a stream-json user message via stdin during the session and verifies the agent receives it
  [verify](tests/loom-test.sh::test_claude_supports_steering)
- [ ] Claude backend shutdown watchdog: on `result` event, loom closes stdin; if claude does not exit within grace period, sends SIGTERM then SIGKILL
  [verify](tests/loom-test.sh::test_claude_shutdown_watchdog)

### Backend selection

- [ ] Per-phase config resolves correct backend (`[agent.todo]` overrides `[agent] default`)
  [verify](tests/loom-test.sh::test_per_phase_backend_config)
- [ ] `--agent` CLI flag overrides all phase config for the invocation
  [verify](tests/loom-test.sh::test_backend_selection_flag)
- [ ] Default (no phase config, no flag) selects claude
  [verify](tests/loom-test.sh::test_backend_default_claude)
- [ ] Invalid backend name produces clear error
  [verify](tests/loom-test.sh::test_backend_invalid_name)
- [ ] Pi backend calls `set_model` after spawn when phase config specifies provider/model
  [verify](tests/loom-test.sh::test_pi_set_model_from_phase_config)

### Container integration

- [ ] Loom spawns containers via `wrapix run-bead --spawn-config <file>
      --stdio` with the correct profile image, never via `podman run` directly
  [verify](tests/loom-test.sh::test_wrapix_run_bead_spawn)
- [ ] Container receives agent stdin/stdout via pipe
  [verify](tests/loom-test.sh::test_container_stdio_pipe)
- [ ] Entrypoint starts pi in RPC mode when `WRAPIX_AGENT=pi`
  [verify](tests/loom-test.sh::test_entrypoint_pi_mode)
- [ ] Entrypoint starts claude normally when `WRAPIX_AGENT=claude`
  [verify](tests/loom-test.sh::test_entrypoint_claude_mode)
- [ ] Entrypoint preserves git SSH, beads, network filtering for both agents
  [verify](tests/loom-test.sh::test_entrypoint_shared_setup)

### Agent runtime layer

- [ ] Pi runtime layer adds Node.js and pi binary to any workspace profile
  [verify](tests/loom-test.sh::test_pi_runtime_layer)
- [ ] Image builds with `profile:rust` + `WRAPIX_AGENT=pi` (composition works)
  [verify](tests/loom-test.sh::test_pi_rust_composition)
- [ ] Image builds with `profile:base` + `WRAPIX_AGENT=pi`
  [verify](tests/loom-test.sh::test_pi_base_composition)
- [ ] Pi binary is functional inside container (`pi --version` succeeds)
  [verify](tests/loom-test.sh::test_pi_binary_in_container)
- [ ] Claude runtime adds nothing (claude already in base image)
  [verify](tests/loom-test.sh::test_claude_runtime_noop)

## Out of Scope

- **Pi-mono extensions** — Loom controls pi via RPC, not via pi's extension
  system. No TypeScript extensions are written or loaded.
- **Pi-mono web-ui** — terminal-only integration.
- **Direct LLM API calls** — Loom delegates to agent binaries which handle
  API communication. No direct Anthropic/OpenAI/etc. API usage.
- **Pi-mono forking or vendoring** — consumed as an npm package bundled by
  Nix. No source-level fork.
- **macOS (Darwin) support for pi runtime layer** — initially Linux
  containers only. Darwin support is a follow-up.
- **Multiple simultaneous backends** — one backend per phase invocation.
  No parallel multi-backend sessions within a single phase.
- **Claude Code RPC mode** — if Anthropic ships an RPC mode for Claude Code,
  the Claude backend can be upgraded. Not designed for now.

## Implementation Notes

### Anthropic subscription policy

As of April 2026, Anthropic no longer allows third-party applications to consume
Claude Pro/Max plan quota. Pi-mono backed by Claude must use API keys with
separate billing. Users who want to use their Max subscription must use the
Claude backend (which runs the `claude` binary directly). This is a policy
constraint, not a technical one — document it in user-facing help text.

### Pi-mono coordinates

The repository is `badlogic/pi-mono` (GitHub) and the npm package is
`@mariozechner/pi-coding-agent`. Earendil's acquisition (April 2026) was
announced but the repository and package have not moved yet. Use the current
canonical coordinates.

### Pi-mono version pinning

As of May 2026, the npm `latest` and the GitHub `main` branch are in lockstep
(both at 0.72.1). Pi-mono ships daily releases — pin to a specific version
in the Nix flake input (`@mariozechner/pi-coding-agent@0.72.1`) and update
deliberately, not automatically. The RPC protocol has no formal versioning —
there is no handshake or protocol version field. Stability is inferred from
the `rpc-types.ts` type definitions in the source. The startup `get_commands`
probe detects breaking changes (missing commands) at session start.

### Claude stream-json format

Claude Code's stream-json is not extensively documented by Anthropic. The Claude
backend should be tested against captured output fixtures and handle unknown
event types gracefully via `#[serde(other)]`. The format may change between
Claude Code versions — pin to tested versions.

### Pi process lifecycle

Pi in `--mode rpc` stays alive between prompts — it's a long-running process.
The driver can send multiple `prompt` / `steer` / `follow_up` commands without
restarting. This differs from Claude Code, which is a single-session process
that exits after completion. The pi backend should handle both "session still
active, send follow-up" and "session complete, process still running" states.

### Pi cost tracking

Pi does not report cost in its event stream. To capture cost data, the driver
can send `get_session_stats` before closing the session — the response includes
`tokens` (input, output, cacheRead, cacheWrite, total), `cost`, and
`contextUsage`. This is optional; `SessionOutcome::cost_usd` is `None` if the
driver does not query stats.

### Timeout and retry

Neither protocol defines explicit timeout semantics. Loom implements its own:

- **Read timeout** — if no NDJSON line arrives within 5 minutes, log a warning.
  Do not abort — long tool executions (builds, tests) are expected.
- **Process exit** — if the child process exits while events are pending, drain
  remaining stdout before reporting `ProcessExit`.
- **Retry** — handled at the workflow level (loom-workflow), not the agent level.
  The agent backend reports failures; the workflow engine decides whether to
  retry.
