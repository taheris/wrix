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
//! build against the surface before the implementations land. Each bead
//! that implements a module replaces the shared scaffold error with its
//! own per-module error enum per RS-4.

use displaydoc::Display;
use thiserror::Error;

pub mod annotation;
pub mod cache;
pub mod dispatch;
pub mod integrity;
pub mod runner;

pub use annotation::{Annotation, Criterion, ParsedSpecs, Tier};
pub use cache::{CacheRow, Verdict};
pub use dispatch::VerifierVerdict;
pub use integrity::{
    CommandResolver, FsCommandResolver, IntegrityError, IntegrityFinding,
    RustWorkspaceTestResolver, TestPathResolver,
};
pub use runner::RunnerTemplate;

/// Scaffold error returned by every stub entry point until the
/// implementation bead lands. Per RS-9, stubs return a typed error rather
/// than panicking via `todo!()` / `unimplemented!()`. Per RS-4, each
/// implementation bead replaces this shared sentinel with its own
/// per-module error enum (`annotation::ParseError`, `cache::CacheError`,
/// …) carrying meaningful failure variants.
#[derive(Debug, Display, Error)]
pub enum GateError {
    /// not yet implemented: {what}
    Unimplemented { what: &'static str },
}
