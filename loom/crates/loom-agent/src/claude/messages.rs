//! Claude Code stream-json protocol message types.
//!
//! Unlike pi, claude's NDJSON messages follow a clean tagged union: every
//! line carries a `type` field that uniquely identifies the variant. Serde's
//! internally-tagged enum handles dispatch directly, with `#[serde(other)]`
//! catching any future variants without breaking the build.
//!
//! Inner content shapes (`assistant`/`user` `message` payloads, `result`
//! tool-output extraction) and the matching parser pass land in wx-pkht8.7
//! — this file declares the outer tagged-enum surface so subsequent passes
//! have stable variant names to reference.

use loom_core::identifier::{RequestId, SessionId};
use serde::Deserialize;

/// Tagged union of every line type emitted by `claude --output-format
/// stream-json`. Discriminated by the `type` field on each NDJSON message.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeMessage {
    /// Session metadata. Subtype `init` carries the `session_id`; other
    /// subtypes are forwarded for debug logging.
    #[serde(rename = "system")]
    System {
        subtype: String,
        session_id: Option<SessionId>,
    },

    /// Assistant turn payload — text deltas and tool-use entries live in
    /// `message`. Body destructuring lands in wx-pkht8.7.
    #[serde(rename = "assistant")]
    Assistant { message: serde_json::Value },

    /// User turn payload — tool results echoed back live in `message`.
    /// Body destructuring lands in wx-pkht8.7.
    #[serde(rename = "user")]
    User { message: serde_json::Value },

    /// Final-result line. `subtype: "success"` maps to `TurnEnd` followed by
    /// `SessionComplete`; `subtype: "error"` maps to `Error` followed by
    /// `SessionComplete`. `total_cost_usd` is captured into
    /// [`SessionOutcome::cost_usd`](loom_core::agent::SessionOutcome::cost_usd).
    #[serde(rename = "result")]
    Result {
        subtype: String,
        result: Option<String>,
        total_cost_usd: Option<f64>,
        duration_ms: Option<u64>,
        num_turns: Option<u32>,
        is_error: Option<bool>,
    },

    /// Tool permission probe. With `--permission-prompt-tool stdio`, claude
    /// emits these and expects a `control_response` on stdin. Loom auto-
    /// approves (sandbox is the trust boundary) but logs every approval at
    /// `info!` for an audit trail.
    #[serde(rename = "control_request")]
    ControlRequest {
        id: RequestId,
        tool: String,
        input: serde_json::Value,
    },

    /// Forward-compatibility catch-all so a new claude message type does not
    /// fail the parse.
    #[serde(other)]
    Unknown,
}
