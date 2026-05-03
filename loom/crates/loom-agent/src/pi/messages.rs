//! Pi-mono RPC protocol message types.
//!
//! Pi messages do not follow a clean tagged-union shape: responses carry
//! `type: "response"` plus an `id`, events carry their own `type` values
//! (`message_update`, `tool_execution_start`, …) without an `id`, and
//! extension UI requests carry `type: "extension_ui_request"`. The parser
//! therefore peeks at `(type, id)` via [`PiEnvelope`] and re-deserializes the
//! line into the matched concrete type.
//!
//! Variant bodies and the matching parser pass land in wx-pkht8.5 — this
//! file declares the type names and the discriminating fields the parser
//! depends on.

use loom_core::identifier::RequestId;
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
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

/// Streaming event from pi (no `id` field). Variants — `message_update`,
/// `tool_execution_start`, `turn_end`, `agent_end`, `compaction_*`, … — and
/// their nested payloads land in wx-pkht8.5.
#[derive(Debug, Deserialize)]
pub struct PiEvent {
    /// Event name (e.g. `"message_update"`, `"tool_execution_start"`).
    #[serde(rename = "type")]
    pub event_type: String,
}

/// Extension UI request (`type: "extension_ui_request"`). Pi extensions
/// drive these; loom auto-cancels response-required methods (`select`,
/// `confirm`, `input`, `editor`) so a missing host reply does not stall
/// the agent. Method enumeration lands in wx-pkht8.5.
#[derive(Debug, Deserialize)]
pub struct PiUiRequest {
    pub id: RequestId,
    pub method: String,
}

/// `prompt` command body — opens the session.
#[derive(Debug, Serialize)]
pub struct PromptCommand {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub message: String,
}

/// `steer` command body — mid-session course correction.
#[derive(Debug, Serialize)]
pub struct SteerCommand {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub message: String,
}

/// `abort` command body — terminates the in-flight operation.
#[derive(Debug, Serialize)]
pub struct AbortCommand {
    #[serde(rename = "type")]
    pub kind: &'static str,
}
