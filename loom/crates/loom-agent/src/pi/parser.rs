//! Pi-mono RPC line parser.
//!
//! The full [`LineParse`](loom_core::agent::LineParse) implementation —
//! two-phase deserialization, event mapping, command encoding, and the
//! `extension_ui_request` auto-cancel reply — lands in wx-pkht8.5.

/// Pi-mono RPC line parser.
///
/// Stateless dispatch layer between
/// [`AgentSession`](loom_core::agent::AgentSession) and
/// [`messages`](super::messages). The parser owns NDJSON framing on stdout
/// (line in → [`ParsedLine`](loom_core::agent::ParsedLine)) and command
/// encoding for stdin (`encode_prompt`/`encode_steer`/`encode_abort`).
pub struct PiParser;
