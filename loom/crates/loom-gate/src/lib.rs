//! `loom-gate` — annotation dispatch, status cache, integrity gate.
//!
//! Owns the runtime that `loom gate <subcommand>` invokes. Walks the
//! consumer's `specs/*.md` for `[tier](target)` annotations (per
//! `docs/spec-conventions.md`), dispatches each annotation to its verifier
//! under the rules in `specs/loom-gate.md`, persists per-criterion verdicts
//! in a sqlite-backed status cache, and self-checks the annotation set via
//! the integrity gate.
//!
//! This crate currently exposes the public type surface only; module-level
//! implementations land in their own beads (annotation parser, runner
//! discovery, status cache, integrity gate, per-tier dispatcher). Stub
//! entry points return [`GateError::Unimplemented`] so dependent crates can
//! build against the surface before the implementations land.

pub mod annotation;
pub mod cache;
pub mod dispatch;
pub mod error;
pub mod integrity;
pub mod runner;

pub use annotation::{Annotation, Tier};
pub use cache::{CacheRow, Verdict};
pub use dispatch::VerifierVerdict;
pub use error::GateError;
pub use integrity::IntegrityFinding;
pub use runner::RunnerTemplate;
