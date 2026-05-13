//! Agent backend implementations for Loom.
//!
//! Houses two zero-sized backends — [`PiBackend`] for pi-mono RPC and
//! [`ClaudeBackend`] for Claude Code stream-json — that implement the
//! [`AgentBackend`](loom_driver::agent::AgentBackend) trait declared in
//! `loom-driver`. The trait's job is process lifecycle only; conversation
//! driving (prompt, steer, abort, event streaming) lives on
//! [`AgentSession`](loom_driver::agent::AgentSession), which both backends
//! return in the `Idle` state.
//!
//! Subsequent issues populate each backend module:
//!
//! - `pi/parser.rs`, `pi/messages.rs` — wx-pkht8.5
//! - `pi/backend.rs` — wx-pkht8.6
//! - `claude/parser.rs`, `claude/messages.rs` — wx-pkht8.7
//! - `claude/backend.rs` — wx-pkht8.8
//!
//! This crate currently exposes the skeleton: the ZST types, the module
//! layout, and `AgentBackend` impls that fail closed with
//! [`ProtocolError::Unsupported`](loom_driver::agent::ProtocolError::Unsupported)
//! so a half-wired call site cannot accidentally drive a real container.

pub mod claude;
pub mod pi;

pub use claude::ClaudeBackend;
pub use pi::PiBackend;
