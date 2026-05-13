//! `loom-events` — the public contract leaf crate.
//!
//! Frontends, SSE bridges, and external log analyzers depend on this
//! crate to consume `AgentEvent` and the domain identifier newtypes
//! without pulling in the `loom-driver` runtime (rusqlite, gix, tokio,
//! …). The Cargo dependency surface is intentionally tiny — `serde`,
//! `serde_json`, `thiserror`. Adding anything else changes the
//! dependency surface of every downstream consumer and requires a spec
//! change.
//!
//! The `loom-driver` crate re-exports the contents of this crate so
//! existing call sites (`use loom_driver::identifier::BeadId`,
//! `use loom_driver::agent::event::AgentEvent`) keep working without
//! churn. New code that doesn't need the runtime should depend on
//! `loom-events` directly.

pub mod event;
pub mod identifier;

pub use event::AgentEvent;
