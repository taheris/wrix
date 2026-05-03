use serde::Serialize;

use crate::identifier::ToolCallId;

/// Backend-neutral event flowing from a running agent up to the workflow
/// engine. Both pi and claude line parsers normalize their wire messages into
/// this enum — once an `AgentEvent` flows downstream no code knows which
/// backend produced it.
///
/// `Serialize` is derived so the on-disk NDJSON log file is the same event
/// stream the terminal renderer consumes (see `logging::LogSink`).
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

    /// Agent finished one turn (a multi-turn session may emit several).
    TurnEnd,

    /// Agent session completed — the underlying process is exiting or the
    /// final result line was observed.
    SessionComplete {
        exit_code: i32,
        cost_usd: Option<f64>,
    },

    /// Agent context compaction has begun.
    CompactionStart { reason: CompactionReason },

    /// Agent context compaction has ended; `aborted` distinguishes "compacted
    /// successfully" from "compaction abandoned".
    CompactionEnd { aborted: bool },

    /// Agent reported an error mid-stream (does not necessarily end the
    /// session — a `SessionComplete` may follow).
    Error { message: String },
}

/// Why the agent compacted its context.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    /// Approaching or exceeded the model context limit.
    ContextLimit,
    /// User (or driver) explicitly requested compaction.
    UserRequested,
    /// Reason was not present or did not match a known value.
    Unknown,
}
