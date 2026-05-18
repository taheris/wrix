//! Per-tier dispatch surface.
//!
//! Routes each [`Annotation`] to its verifier per the verifier-runner
//! contract in `specs/loom-gate.md`. `[check]` and `[system]` annotations
//! get one subprocess each; `[test]` and `[judge]` annotations collect
//! into batched invocations (one runner subprocess for `[test]`,
//! concurrent LLM calls for `[judge]`). Dispatcher implementation lands
//! in a later bead — this module exposes the typed verdict surface and a
//! stub entry point.

use serde::{Deserialize, Serialize};

use crate::GateError;
use crate::annotation::Annotation;

/// JSON-line verdict every verifier returns on stdout, per the
/// verifier-runner contract in `specs/loom-gate.md`. The exit code mirrors
/// `pass` (0 for true, non-zero for false); the gate parses one line of
/// JSON-encoded `VerifierVerdict` from each verifier's stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifierVerdict {
    pub pass: bool,
    pub evidence: String,
}

/// Dispatch `annotations` per their tier and return one verdict per
/// criterion. Stub — implemented in the per-tier dispatcher bead.
pub fn run(_annotations: &[Annotation]) -> Result<Vec<VerifierVerdict>, GateError> {
    Err(GateError::Unimplemented {
        what: "dispatch::run",
    })
}
