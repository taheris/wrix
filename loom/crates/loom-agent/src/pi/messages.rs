//! Pi-mono RPC protocol message types.
//!
//! Pi messages do not follow a clean tagged-union shape: responses carry
//! `type: "response"` plus an `id`, events carry their own `type` values
//! (`message_update`, `tool_execution_start`, …) without an `id`, and
//! extension UI requests carry `type: "extension_ui_request"`. The parser
//! therefore peeks at `(type, id)` via [`PiEnvelope`] and re-deserializes the
//! line into the matched concrete type.

use loom_core::identifier::{RequestId, ToolCallId};
use serde::{Deserialize, Serialize};

/// First-pass peek of a pi NDJSON line. Carries only the discriminating
/// fields; the parser re-deserializes into the appropriate concrete type
/// once the message category is known.
#[derive(Debug, Deserialize)]
pub struct PiEnvelope {
    /// Either `"response"`, `"extension_ui_request"`, or one of the event
    /// names. `None` for pathological lines that survive JSON parse without
    /// a `type` field.
    #[serde(rename = "type")]
    pub msg_type: Option<String>,

    /// Present on responses and extension UI requests; absent on events.
    /// The parser uses id-absence as the fallback that classifies the
    /// remainder as events.
    pub id: Option<RequestId>,
}

/// Response envelope — one of these is emitted for every command sent on
/// stdin. The `command` field echoes back the command name; `success`
/// discriminates between a successful `data` payload and a failure carried
/// in `error`.
#[derive(Debug, Deserialize)]
pub struct PiResponse {
    pub id: RequestId,
    pub command: String,
    pub success: bool,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Streaming event from pi (no `id` field). Discriminated by the wire
/// `type` value via serde's internally-tagged enum form. Variants whose
/// payload Loom does not consume (turn boundaries, retry telemetry,
/// extension errors) are unit forms — serde drops their extra fields.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PiEvent {
    /// Streaming assistant message update; the inner
    /// [`AssistantMessageDelta`] dispatch determines what (if anything) is
    /// emitted as an [`AgentEvent`](loom_core::agent::AgentEvent).
    MessageUpdate {
        #[serde(rename = "assistantMessageEvent")]
        delta: AssistantMessageDelta,
    },

    /// Pi started executing a tool call.
    ToolExecutionStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: ToolCallId,
        #[serde(rename = "toolName")]
        tool_name: String,
        #[serde(default)]
        args: serde_json::Value,
    },

    /// Pi finished executing a tool call.
    ToolExecutionEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: ToolCallId,
        #[serde(default)]
        result: serde_json::Value,
        #[serde(default, rename = "isError")]
        is_error: bool,
    },

    /// Streaming tool-call progress update — observability only.
    ToolExecutionUpdate,

    /// Turn boundaries — payload is dropped.
    TurnStart,
    TurnEnd,

    /// Agent lifecycle.
    AgentStart,
    AgentEnd,

    /// Compaction lifecycle. The reason string is one of `"threshold"`,
    /// `"overflow"`, `"manual"` as of pi v0.72.
    CompactionStart {
        #[serde(default)]
        reason: Option<String>,
    },
    CompactionEnd {
        #[serde(default)]
        aborted: bool,
    },

    /// Per-stream queue change — observability only.
    QueueUpdate,

    /// Auto-retry telemetry — observability only.
    AutoRetryStart,
    AutoRetryEnd,

    /// Extension reported an error — observability only.
    ExtensionError,

    /// Forward-compatibility catch-all so a new pi event type does not
    /// fail the parse. Logged at trace level by the parser.
    #[serde(other)]
    Unknown,
}

/// Inner `assistantMessageEvent` delta carried by
/// [`PiEvent::MessageUpdate`]. Dispatched on the nested `type` field —
/// most variants are observability-only; only `text_delta` and `error`
/// surface as [`AgentEvent`](loom_core::agent::AgentEvent)s.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageDelta {
    /// Streaming text fragment.
    TextDelta { text: String },

    /// Mid-stream error from the agent. Pi populates `reason`
    /// (`"aborted"` / `"error"`) and may include a `message` with
    /// human-readable detail.
    Error {
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },

    /// Forward-compatibility catch-all for delta types Loom does not
    /// consume (`start`, `text_start`, `text_end`, `thinking_*`,
    /// `toolcall_*`, `done`, …). Logged at trace level by the parser.
    #[serde(other)]
    Unknown,
}

/// Extension UI request (`type: "extension_ui_request"`). Loom replies
/// with an auto-cancel for response-required methods (`select`,
/// `confirm`, `input`, `editor`); methods that do not need a response
/// (`notify`, `setStatus`, `setWidget`, `setTitle`, `set_editor_text`)
/// are skipped silently.
#[derive(Debug, Deserialize)]
pub struct PiUiRequest {
    pub id: RequestId,
    pub method: String,
}

/// Auto-cancel reply for [`PiUiRequest`] methods that block the agent
/// awaiting a host response. The shape matches pi's `extension_ui_response`
/// — `cancelled: true` tells the extension the host declined.
#[derive(Debug, Serialize)]
pub struct ExtensionUiResponse<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: &'a RequestId,
    pub cancelled: bool,
}

/// `prompt` command body — opens the session.
#[derive(Debug, Serialize)]
pub struct PromptCommand<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub message: &'a str,
}

/// `steer` command body — mid-session course correction.
#[derive(Debug, Serialize)]
pub struct SteerCommand<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub message: &'a str,
}

/// `abort` command body — terminates the in-flight operation.
#[derive(Debug, Serialize)]
pub struct AbortCommand {
    #[serde(rename = "type")]
    pub kind: &'static str,
}
