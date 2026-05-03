//! Agent backend abstraction surface owned by `loom-core`.
//!
//! Defines the public types and traits that backends in `loom-agent`
//! implement and that `loom-workflow` orchestrates over:
//!
//! - [`AgentBackend`] — minimal trait whose only job is to spawn an
//!   [`AgentSession`] in the [`Idle`] state.
//! - [`AgentSession`] — typestate-guarded conversation handle (`Idle` →
//!   `Active` after `prompt`).
//! - [`AgentEvent`] — the backend-neutral event enum the session yields.
//! - [`LineParse`] / [`ParsedLine`] — backend-specific protocol bridge: line
//!   parsing plus command encoding for prompt/steer/abort.
//! - [`NdjsonReader`] — shared stdin line framing (10 MB max line cap).
//! - [`RePinContent`] — re-pin payload format used by both backends after
//!   compaction.
//! - [`SpawnConfig`] / [`SessionOutcome`] — the contract between loom and
//!   `wrapix run-bead`.
//!
//! Protocol-parsing of backend-specific message types (`PiMessage`,
//! `ClaudeMessage`) lives in `loom-agent`, not here.

mod backend;
mod error;
mod event;
mod kind;
mod ndjson;
mod parse;
mod repin;
mod session;

pub use backend::{AgentBackend, ModelSelection, SessionOutcome, SpawnConfig};
pub use error::ProtocolError;
pub use event::{AgentEvent, CompactionReason};
pub use kind::AgentKind;
pub use ndjson::{MAX_LINE_BYTES, NdjsonReader};
pub use parse::{LineParse, ParsedLine};
pub use repin::RePinContent;
pub use session::{Active, AgentSession, Idle};
