//! Claude Code stream-json line parser.
//!
//! The full [`LineParse`](loom_core::agent::LineParse) implementation —
//! `ClaudeMessage` dispatch, `result/success` → (`TurnEnd`, `SessionComplete`)
//! pair, `control_request` auto-approve flow, and stream-json user-message
//! encoding — lands in wx-pkht8.7.

/// Claude Code stream-json line parser.
///
/// Stateless dispatch layer between
/// [`AgentSession`](loom_core::agent::AgentSession) and
/// [`messages::ClaudeMessage`](super::messages::ClaudeMessage).
pub struct ClaudeParser;
