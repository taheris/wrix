//! Loom workflow engine.
//!
//! Implements the workflow phases (`plan`, `todo`, `run`, `check`, `msg`,
//! `spec`) on top of `loom-core`'s typed surface and `loom-templates`'
//! Askama-rendered prompts. Subsequent issues populate each phase module;
//! this crate currently exposes the skeleton only.
//!
//! The agent surface from `loom-core` (`AgentBackend`, `AgentEvent`,
//! `AgentSession`, `RePinContent`, `SpawnConfig`) is re-exported through
//! this module index so workflow phases can import the symbols without
//! depending on `loom-core` directly each time.

pub mod run;
pub mod todo;

pub use loom_core::agent::{
    Active, AgentBackend, AgentEvent, AgentKind, AgentSession, CompactionReason, Idle, LineParse,
    MAX_LINE_BYTES, NdjsonReader, ParsedLine, ProtocolError, RePinContent, SessionOutcome,
    SpawnConfig,
};
