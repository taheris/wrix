# Loom Agent

Agent backend abstraction, pi-mono and Claude Code implementations, container
communication, and agent runtime layer for the pi runtime.

## Problem Statement

Ralph's bash scripts launch Claude Code as the only agent runtime, creating
vendor lock-in to Anthropic. Users with a Claude Max subscription need the
`claude` binary; users who want LLM-agnostic switching need an alternative.
Pi-mono provides 20+ LLM provider backends and an JSONL RPC mode that enables
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
   spawns per-bead containers by invoking `wrapix spawn --spawn-config
   <file> --stdio` (a thin wrapix subcommand that owns container construction)
   and communicates with the agent process inside via stdin/stdout pipes.
   Loom never calls `podman run` directly; see
   [loom-harness.md — Process Architecture](loom-harness.md#process-architecture).
2. **Agent backend trait** — an async Rust trait (`AgentBackend`) abstracting
   agent lifecycle: spawn a session. Used via type parameter
   (`<B: AgentBackend>`) — the concrete backend is known at each call site.
3. **Pi backend** — speaks pi-mono's JSONL RPC protocol over stdin/stdout.
   Commands:
   - `prompt` — send initial or follow-up prompts
   - `steer` — mid-session course correction
   - `abort` — terminate current operation
   - `set_thinking_level` — adjust reasoning effort (best-effort: not
     included in the startup `get_commands` probe; sent only when the
     phase config requests it, and silently skipped if pi rejects it)
   - `set_model` — switch LLM provider/model mid-session

   Plus streaming event parsing for message deltas, tool calls, tool results,
   completion, compaction, and errors.
4. **Claude backend** — launches
   `claude --print --input-format stream-json --output-format stream-json`,
   parses JSONL events from stdout, and writes user messages (initial
   prompt, steering) as stream-json on stdin. `--permission-prompt-tool
   stdio` enables the `control_request` / `control_response` flow. The
   `--print` flag keeps the session non-interactive (runs to completion,
   exits) while `--input-format stream-json` enables mid-session steering
   via additional user messages. On observing a `result` event, loom closes
   its end of stdin, waits `[claude] post_result_grace_secs` (default 5s)
   for natural exit, then escalates SIGTERM → SIGKILL.
5. **Per-phase backend selection** — each workflow phase (plan, todo, run,
   check, msg) independently resolves its backend and model from config.
   `[phase.default].agent.backend` sets the fallback (`claude`). Per-phase
   overrides (e.g. `[phase.todo]`) carry `agent.backend` plus optional
   `agent.provider` and `agent.model_id`. `--agent` CLI flag overrides all
   phase config for the current invocation.
6. **Agent runtime layer** — the image builder composes two orthogonal axes:
   *workspace profile* (base, rust, python) and *agent runtime* (claude, pi).
   When `WRAPIX_AGENT=pi`, the pi runtime layer (Node.js + pi binary) is
   added to whichever workspace profile is configured for the bead. No
   standalone `profiles.pi` — no profile proliferation.
7. **Entrypoint agent selection** — `entrypoint.sh` checks `WRAPIX_AGENT` and:
   - `claude` (default): existing behavior (Claude config merging, hooks,
     `claude --dangerously-skip-permissions`)
   - `pi`: skips Claude-specific config, starts `pi --mode rpc` listening on
     stdin/stdout
8. **Event normalization** — both backends emit a common `AgentEvent` enum so
   the workflow engine does not need backend-specific event handling.
9. **JSONL framing** — both protocols use JSON Lines (one complete
   JSON object per line, separated by `\n`). The JSONL reader splits on `\n`
   only, not Unicode line separators (U+2028, U+2029). Each line is
   independently parseable.

### Non-Functional

1. **No podman socket mounting** — `wrapix spawn` invokes podman on the
   host; the agent runs inside the resulting container with no access to the
   podman socket. No nested container support needed.
2. **Graceful degradation** — if a backend-specific feature is unavailable
   (e.g. pi providers that don't support `set_thinking_level`, or pi
   builds where manual `compact` is disabled), the driver continues
   without it. No hard failures for missing optional capabilities.
3. **Parse, Don't Validate** — raw protocol bytes are parsed into typed domain
   representations at the JSONL boundary. All code downstream of the parser
   works with already-validated types. No re-parsing, no stringly-typed event
   matching.
4. **Static dispatch** — `AgentBackend` uses an explicit type parameter
   (`<B: AgentBackend>`), not a trait object. Backends are zero-sized types
   with associated functions (no `&self`). A `dispatch` function in the
   binary crate matches on `AgentKind` per phase and calls
   `run_agent::<ConcreteType>`. No `async-trait` needed — `async fn` in
   traits is stable and works directly with static dispatch.

## Architecture

Throughout this section, **"driver"** refers to loom's backend-side code that
drives the agent process over JSONL — distinct from the agent (pi or claude)
running inside the container.

### Dispatch: ZST Backends + Per-Phase Selection

Backends are zero-sized types — `PiBackend` and `ClaudeBackend`. All
runtime state lives in the session and `SpawnConfig`; the backend type
parameter alone carries dispatch. No instances, no constructor.

The backend is resolved **per phase** from config, not once at startup.
Each workflow command (plan, todo, run, check, msg) independently selects
its backend + model. The binary crate exposes a single `dispatch`
function that matches on the per-phase choice and forwards to a generic
helper parameterized by backend type. The workflow engine receives that
helper as a parameter and never touches concrete backend types — static
dispatch is preserved inside each match arm.

**Per-phase config example:** `loom todo` uses a cheap model via pi,
while `loom review` uses claude directly:

```toml
[phase.default]
agent.backend = "claude"

[phase.todo]
agent.backend = "pi"
agent.provider = "deepseek"
agent.model_id = "deepseek-v3"

[phase.review]
agent.backend = "claude"
```

Phases without explicit config inherit `[phase.default]`. The pi backend
calls `set_model` after spawn if the phase config specifies a
provider/model.

`[phase.plan]` is also a valid per-phase key, but the resolution path
differs: `loom plan` is interactive (human-in-the-loop) and shells to
the backend's interactive entry point rather than going through the
agent-backend abstraction. Today only claude is wired up there, via
`wrapix run`; pi would need an interactive frontend before it could be
selected for `plan`.

Mock backends slot into the same dispatch — they are ZSTs too, so a
test-time entry parameterized with `MockBackend` works without any
production code change.

### Agent Backend Trait

The agent-backend abstraction is deliberately minimal: it exposes a
single asynchronous `spawn` operation that consumes a `SpawnConfig` and
yields an idle session. Process lifecycle is its only concern. Session
interaction (prompt, steer, abort, event streaming) lives on the session
type, not the backend trait — the backend's job is to spawn a session;
the session's job is to drive the conversation.

Both backends support steering: pi via the native `steer` command,
claude via `--input-format stream-json --output-format stream-json`
(sends a stream-json user message on stdin during the session). There is
no capability gate — steering works for both backends. If a future
backend cannot support steering, a capability constant can be
reintroduced.

Backends carry no per-instance state — the type parameter conveys all
information. The implementation uses native `async fn` in traits
(edition 2024) with static dispatch, avoiding the `async-trait` crate.

### Typestate Session

Invalid protocol transitions are compile errors. A session is in one of
two states — **idle** or **active** — and the state is encoded in the
session's type parameter. Operations only available in one state are not
even callable in the other.

State-machine rules:

- **Idle session** must be prompted before events can be read. The
  prompt operation consumes the idle session and yields an active one.
- **Active session** exposes `next_event`, `steer`, and `abort`. It
  cannot be prompted again — only completed or aborted.
- **Aborting** returns to idle: if the backend has a wire abort command
  (pi), the parser encodes it and the session is reusable; if not
  (claude), the typestate still returns to idle but the underlying
  process is left to backend-level shutdown (SIGTERM/SIGKILL via the
  watchdog), so a follow-up prompt fails with a process-exit error.

The session type and the parser abstraction both live in `loom-core` —
not in `loom-agent` — because the agent-backend trait returns a session,
and the inverse dependency would be a cycle.

A single inbound protocol line can yield multiple events. The session
buffers excess events and returns one per call to `next_event`. A
state-agnostic accessor lets backends borrow the underlying child
process without surrendering session ownership; this is the hook the
claude backend's shutdown watchdog uses to drive the SIGTERM/SIGKILL
escalation described in requirement #4.

**Line parsing.** Each backend provides a parser that owns **both
directions of the wire** — decoding inbound JSONL lines into events, and
encoding outbound commands (initial prompt, steer, abort) for stdin. The
parser is held internally by the session via dynamic dispatch so the
session itself stays a single concrete type, free of backend generics.
Static dispatch on the outer agent-backend layer plus dynamic dispatch
on the inner parser layer is the deliberate split — the per-line vtable
call is negligible next to the IO cost of reading from a subprocess
pipe.

The parser's decoded output carries two fields: a list of events to
yield, and an optional response string the session should write back to
stdin before yielding those events. The response slot handles protocol
control flow such as claude's `control_request` auto-approve: the
parser populates the field; the session does the IO. The list of events
is a list (not a single event) because some inbound messages map to
multiple events — claude's `result/success` produces `TurnEnd` +
`SessionComplete`; `result/error` produces `Error` + `SessionComplete`.
Pi's `turn_end` and `agent_end` are separate inbound events that each
map to one outbound event.

### AgentEvent

The session emits a stream of typed events. Event names are part of the
wire format: they are serialized as snake_case (`message_delta`,
`tool_call`, …) when the terminal renderer and on-disk JSONL log share
the tee-style sink (see [loom-harness — Run UX &
Logging](loom-harness.md#run-ux--logging)). Log readers consume those
names directly.

| Event | Payload | Meaning |
|-------|---------|---------|
| `message_delta` | `text` | Streaming text fragment from the agent. |
| `tool_call` | `id`, `tool`, `params` | Agent invoked a tool. |
| `tool_result` | `id`, `output`, `is_error` | Tool execution completed. |
| `turn_end` | — | One turn finished; the session may have more turns. |
| `session_complete` | `exit_code`, `cost_usd?` | Process exiting or final result received. |
| `compaction_start` | `reason` | Context compaction beginning. |
| `compaction_end` | `aborted` | Compaction finished. |
| `error` | `message` | Agent reported an error. |

`reason` is one of `context_limit`, `user_requested`, or `unknown`. Pi's
`threshold` and `overflow` both map to `context_limit`; `manual` maps to
`user_requested`.

The session's terminal outcome — exit code and optional reported cost —
flows out via the `session_complete` event; nothing further is needed
from the workflow engine to learn how a session ended.

### ProtocolError

Operations against an active session can fail with one of a small,
closed set of error categories:

- **Invalid JSON** on a protocol line — the inbound line did not parse.
- **Unknown message type** — JSON parsed, but the discriminator is one
  the backend does not recognize.
- **IO error** — the underlying stdin/stdout channel returned a system
  IO failure.
- **Process exit** — the agent process terminated; the captured exit
  code is reported.
- **Unexpected EOF** — the event stream ended without a terminating
  `session_complete`.
- **Line too long** — an inbound JSONL line exceeded the framing budget
  (10 MB; see [JSONL Framing](#jsonl-framing)).
- **Unsupported** — the backend cannot perform the requested operation
  (e.g., a future backend without steering).

These are the only protocol-level error classes. Backend-specific
failures (model errors, container teardown failures, etc.) surface
through the same channel and are mapped onto these categories at the
parser boundary.

### SpawnConfig

The harness writes a `SpawnConfig` to a JSON file at dispatch time, and
`wrapix spawn --spawn-config <file>` reads it back. This is the single
serialization boundary between loom and the wrapper — preferred over a
fat argv interface. The wrapper's JSON shape is the stable contract;
loom and `wrapix spawn` ship from the same flake and stay in lockstep.

Required fields:

- `image_ref` — podman image reference (e.g. `localhost/wrapix-rust:<hash>`).
- `image_source` — Nix store path the launcher hands to `podman load`
  to materialize that ref.
- `workspace` — host path bind-mounted into the container at
  `/workspace`.
- `env` — explicit env allowlist (table below); the host environment is
  never inherited wholesale.
- `initial_prompt` — prompt rendered from the phase template.
- `agent_args` — extra argv to pass to the agent binary.
- `scratch_dir` — per-key scratch directory the agent backend reads on
  compaction events; see [loom-harness.md § Compaction
  Recovery](loom-harness.md#compaction-recovery).

`image_ref` and `image_source` come from the profile-image manifest at
dispatch time — see [loom-harness.md — Profile-Image
Manifest](loom-harness.md#profile-image-manifest).

The env allowlist is constructed by the workflow engine from
known-needed variables:

| Variable | When | Purpose |
|----------|------|---------|
| `WRAPIX_AGENT` | always | Agent selection in entrypoint |
| `CLAUDE_CODE_OAUTH_TOKEN` | claude backend | Claude authentication |
| `ANTHROPIC_API_KEY` | pi backend (Anthropic models) | LLM API key |
| `TERM` | always | Terminal capability |
| `BEADS_DOLT_SERVER_SOCKET`, `BEADS_DOLT_AUTO_START` | always | Beads dolt-socket path (set to the bind-mount); auto-start disabled (host owns the server) |
| `LOOM_INSIDE` | always | Set to `1`; trips the nested-loom guard if the agent invokes `loom` inside the container — see [loom-harness.md — Nested-Loom Guard](loom-harness.md#nested-loom-guard) |

Provider-specific API keys for the pi backend (OpenAI, Google, etc.) are
added only when the configured model requires them. Variable names are
logged at `info!` level during spawn; values are never logged.

### Host-to-Container Communication

```
loom (host)                                            container
    │                                                       │
    ├─ serialize SpawnConfig → /tmp/loom-<id>.json          │
    ├─ wrapix spawn --spawn-config <file> --stdio        │
    │   └─ exec podman run [no TTY, stdio piped] ─►  entrypoint.sh
    │                                                       │
    │                                                  agent (pi --mode rpc / claude)
    │   stdin ──────────────────────────────────────►  agent
    │   stdout ◄──────────────────────────────────────  agent
    │                                                       │
    ├─ writes JSONL to stdin ────────────────────────►  agent processes command
    │   (both backends)                                     │
    │                                                       │
    ├─ reads JSONL from stdout ◄─────────────────────  agent streams events
    │   (both backends)                                     │
    │                                                       │
    └─ on exit: container teardown via wrapix              │
```

The wrapper hides container construction (mounts, env allowlist, krun
runtime, network filter, deploy key, beads dolt socket) so loom owns only
JSONL framing and the typed `SpawnConfig` it serializes. Both backends use
bidirectional JSONL over stdin/stdout. The Claude backend uses
`--input-format stream-json --output-format stream-json` for full
bidirectional support.

### JSONL Framing

JSON Lines (JSONL, also known as NDJSON): each line is one complete JSON object,
terminated by `\n` (0x0A). Both pi RPC and Claude stream-json use this framing.

Parsing rules:

- Split on `\n` (0x0A). Trailing `\r` is stripped.
- U+2028 and U+2029 are NOT line terminators — they pass through as JSON content.
- Empty lines (blank between objects) are silently skipped.
- Each non-empty line is independently parsed as JSON.
- A line that fails JSON parsing is an "invalid JSON" protocol error.

A per-line byte budget of **10 MB** prevents a malicious or
malfunctioning agent from exhausting host memory by sending a single
line without a `\n` terminator. 10 MB is well above any legitimate JSONL
message (the largest are tool results with file contents). The limit is
checked after the read completes — the reader will still buffer the full
line, but the error fires before the line is parsed or accumulated
further.

### Pi-Mono RPC Protocol

Pi's `--mode rpc` uses JSONL over stdin/stdout. The protocol has no version
negotiation or handshake. After spawning the process, the Pi backend sends a
`get_commands` probe and verifies the response contains every command Loom
depends on. The probe set is always `prompt`, `steer`, `abort`, `set_model`,
optionally extended with `compact` (if manual compaction is wired up) and
`get_session_stats` (if cost capture is enabled — see
[Pi cost tracking](#pi-cost-tracking)). If the response shape is unexpected
or a required command is missing, the backend fails fast with a clear
version-mismatch error before any workflow begins. After the probe succeeds,
normal command flow starts.

Messages are classified by **two-phase deserialization**: peek at the
`type` and `id` fields to determine the message category, then
deserialize the full payload into the correct type.

The classifier rules:

- A `type` of `"response"` → a response message (carries an `id`).
- A `type` of `"extension_ui_request"` → a UI extension request.
- Any other line lacking an `id` → an event (events carry their own
  `type` values like `"message_update"` and never have an `id`).
- Any other line with an `id` but an unrecognized `type` → an
  unknown-message-type protocol error.

**Why two-phase?** Pi messages don't follow a clean tagged union:
responses have `type: "response"` plus an `id`, events carry their own
`type` values without an `id`, and extension UI requests have a distinct
`type`. The discriminant is `type` for the known message names, with
id-absence as the fallback for events — a two-field dispatch that
serde's built-in tag/content support can't express. The envelope parse
is cheap (unknown fields are skipped); the second parse deserializes
into the exact target type.

**Response envelope.** Every response carries `id`, `command`, `success`,
optional `data` (success payload), and optional `error` (failure
message). The `command` field echoes back the command name. The
`success` boolean discriminates between a successful result (payload in
`data`) and a failure (message in `error`); the driver checks `success`
before accessing `data`.

**Commands (driver → pi, via stdin):**

All commands are JSONL objects with a `type` field. Every command supports an
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
| `agent_end` | `messages` | `AgentEvent::SessionComplete` (`exit_code: 0`, synthesized) — see note below |
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
sending another command. The mapping assumes one prompt per container; pi's
`agent_end` carries no exit code, so loom synthesizes `0`.

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
`setTitle`/`set_editor_text`) are logged and ignored. The auto-cancel reply
is built inside `PiParser::parse_line`, which populates `ParsedLine::response`
with the encoded `extension_ui_response` line so the runner just writes it
back to the agent's stdin — no policy lives in the workflow layer.

**Stdout discipline:** Pi v0.72+ calls `takeOverStdout()` at RPC startup —
`process.stdout.write` is monkey-patched to redirect non-protocol writes to
stderr, while protocol output uses a captured raw fd via `writeRawStdout`
(see `output-guard.ts`). Extensions, libraries, and OSC escape sequences
cannot corrupt the protocol stream. The earlier corruption issue
([pi-mono#2388](https://github.com/badlogic/pi-mono/issues/2388)) was fixed
in March 2026. The JSONL parser's malformed-line handling (log warning,
skip line) is retained as defensive coding, not a live mitigation.

### Claude Stream-JSON Protocol

Claude Code's `--output-format stream-json` emits JSONL events.
Combined with `--input-format stream-json`, communication is bidirectional.

**Events (claude → driver, via stdout).** Unlike pi, claude messages
follow a clean tagged union: every message has a `type` field that
uniquely identifies the variant, so a single-pass deserialization with
the `type` field as discriminator suffices (no two-phase classifier
needed). The wire types and their payload shapes:

| `type` | Payload |
|--------|---------|
| `system` | `subtype`, optional `session_id` |
| `assistant` | message content (text or tool_use) |
| `user` | message content (tool_result) |
| `result` | `subtype`, optional `result`, optional `total_cost_usd`, optional `duration_ms`, optional `num_turns`, optional `is_error` |
| `control_request` | `id`, `tool`, `input` |

Any `type` value the parser does not recognize is logged at `debug!` and
skipped — claude can introduce new event types without breaking the
session.

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

**Deny-list.** A configurable `denied_tools` list under `[security]` in
`config.toml` (e.g. `denied_tools = ["WebFetch"]`) rejects specific tool
names with `approved: false`. Empty by default — the container sandbox is
the trust boundary and logging is the primary mitigation. The slot exists
today so a deny rule can be added without a loom release if Claude Code
ships a tool type that reaches outside the container boundary.

### Compaction Handling

The harness creates a per-session scratch directory containing the
rendered prompt and a live scratchpad — see [loom-harness.md § Compaction
Recovery](loom-harness.md#compaction-recovery) for the file layout and
lifecycle. This section describes only how each backend delivers the
recovery content to the agent.

**Delivery is asymmetric** because claude stream-json does not expose
compaction events — Anthropic compacts internally with no protocol
notification, so claude must use its own `SessionStart` hook system, while
pi exposes compaction events natively in JSONL. The asymmetry is
fundamental to the products' compaction models, not a Loom design choice.

**Claude backend:**
- Before spawn, the harness writes `repin.sh` and a `claude-settings.json`
  fragment registering it under `SessionStart[matcher: compact]` into the
  container's runtime directory.
- Claude Code's hook system runs `repin.sh` on each compaction; the
  script emits a JSON envelope assembled from the scratch directory's
  `prompt.txt` and `scratch.md`. The driver is not involved at compaction
  time.

**Pi backend:**
- Knows the per-key scratch directory path from the harness's
  `SpawnConfig`.
- When a `compaction_start` event arrives in the JSONL stream, reads
  `prompt.txt` + `scratch.md` from the scratch directory and sends the
  concatenated content via a `steer` command on stdin.
- **Steer timing.** A `steer` command queues; pi delivers it after the
  current assistant turn finishes its tool calls, before the next LLM
  call — it does not inject content during compaction itself. The re-pin
  therefore reaches the agent on the *next* turn after compaction
  completes, which is the desired effect (post-compact context
  restoration).
- **Overflow auto-retry.** When `compaction_start.reason == "overflow"`
  and compaction succeeds, pi automatically retries the prompt
  (`compaction_end.willRetry == true`). A steer queued during this window
  interleaves with the auto-retry: it lands on the turn following the
  retry's first response, not before. This is acceptable — the retry
  plus re-pin combined still restore working context — but documented so
  the behavior is not surprising in logs.
- The subsequent `compaction_end` event confirms whether compaction
  succeeded (`aborted: false`) or was abandoned. If pi retries compaction
  (a fresh `compaction_start` arrives), the driver re-reads the scratch
  directory and re-pins again — the scratchpad may have grown between
  compactions.

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
- [ ] `SpawnConfig` struct captures image_ref, image_source, workspace, env, initial_prompt, agent_args, scratch_dir
  [verify](tests/loom-test.sh::test_spawn_config_fields)
- [ ] Typestate `AgentSession<Idle>` / `AgentSession<Active>` prevents invalid transitions
  [verify](tests/loom-test.sh::test_typestate_transitions)
- [ ] `ProtocolError` variants cover InvalidJson, UnknownMessageType, Io, ProcessExit, UnexpectedEof, LineTooLong, Unsupported
  [verify](tests/loom-test.sh::test_protocol_error_variants)

### Pi backend

- [ ] Pi backend sends `get_commands` probe on startup and fails fast if required commands are missing
  [verify](tests/loom-test.sh::test_pi_startup_probe)
- [ ] Pi backend parses JSONL events via two-phase deserialization
  [verify](tests/loom-test.sh::test_pi_two_phase_deser)
- [ ] Pi backend sends JSONL commands to pi's stdin
  [verify](tests/loom-test.sh::test_pi_rpc_command_sending)
- [ ] Pi backend supports steering (steer returns Ok and reaches the agent on the next turn)
  [verify](tests/loom-test.sh::test_pi_supports_steering)
- [ ] Pi backend maps all pi event types to AgentEvent variants
  [verify](tests/loom-test.sh::test_pi_event_mapping)
- [ ] Pi backend detects CompactionStart event, reads `prompt.txt` + `scratch.md` from the per-key scratch directory, and sends the concatenated content via steer
  [verify](tests/loom-test.sh::test_pi_compaction_repin)
- [ ] Pi backend handles malformed JSONL gracefully (logs warning, continues)
  [verify](tests/loom-test.sh::test_pi_malformed_jsonl)
- [ ] Pi backend logs extension_ui_request at debug level without responding
  [verify](tests/loom-test.sh::test_pi_extension_ui_passthrough)

### Claude backend

- [ ] Claude backend parses stream-json JSONL events from claude's stdout
  [verify](tests/loom-test.sh::test_claude_stream_json_parsing)
- [ ] Claude backend uses `#[serde(tag = "type")]` for tagged enum deserialization
  [verify](tests/loom-test.sh::test_claude_tagged_enum)
- [ ] Claude backend maps claude event types to AgentEvent variants
  [verify](tests/loom-test.sh::test_claude_event_mapping)
- [ ] Claude backend captures cost_usd from result events
  [verify](tests/loom-test.sh::test_claude_cost_capture)
- [ ] Claude backend handles unknown event types via `#[serde(other)]`
  [verify](tests/loom-test.sh::test_claude_unknown_events)
- [ ] Claude backend's `repin.sh` is registered under `SessionStart[matcher: compact]` before spawn, and the script emits a JSON envelope containing the scratch directory's `prompt.txt` + `scratch.md` when fired
  [verify](tests/loom-test.sh::test_claude_repin_hook_registered)
- [ ] Claude backend auto-approves permission requests via control_response
  [verify](tests/loom-test.sh::test_claude_permission_autoapprove)
- [ ] Claude backend supports steering — sends a stream-json user message via stdin during the session and verifies the agent receives it
  [verify](tests/loom-test.sh::test_claude_supports_steering)
- [ ] Claude backend shutdown watchdog: on `result` event, loom closes stdin; if claude does not exit within grace period, sends SIGTERM then SIGKILL
  [verify](tests/loom-test.sh::test_claude_shutdown_watchdog)

### Backend selection

- [ ] Per-phase config resolves correct backend (`[phase.todo].agent.backend` overrides `[phase.default].agent.backend`)
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

- [ ] Loom spawns containers via `wrapix spawn --spawn-config <file>
      --stdio` with the correct profile image, never via `podman run` directly
  [verify](tests/loom-test.sh::test_wrapix_spawn_dispatch)
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
  No mixing of backends within a single phase; parallel sessions all use
  the backend resolved for that phase.
- **Claude Code RPC mode** — if Anthropic ships an RPC mode for Claude Code,
  the Claude backend can be upgraded. Not in scope today.
