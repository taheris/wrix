//! Annotation integrity gate surface.
//!
//! Runs as part of `loom gate check`. Two directions per
//! `specs/loom-gate.md`: forward (every annotation's target is valid for
//! its tier) and atomic-acceptance (each criterion carries exactly one
//! annotation). Implementation lands in the integrity-gate bead — this
//! scaffold exposes the [`IntegrityFinding`] shape and a stub entry point.

use std::path::PathBuf;

use crate::GateError;
use crate::annotation::{Annotation, Tier};

/// One finding surfaced by the integrity gate.
///
/// The two variants line up with the two directions the gate enforces:
/// forward resolution (annotation target invalid for its tier) and atomic
/// acceptance (criterion carries more than one annotation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityFinding {
    /// Annotation's target does not resolve for its tier.
    UnresolvedAnnotation {
        spec: PathBuf,
        line: u32,
        tier: Tier,
        target: String,
    },
    /// Criterion carries more than one annotation; atomic-acceptance
    /// violated. `count` is the number of annotations attached.
    MultipleAnnotations {
        spec: PathBuf,
        line: u32,
        count: usize,
    },
}

/// Run the integrity gate over `annotations`. Stub — implemented in the
/// integrity-gate bead.
pub fn check(_annotations: &[Annotation]) -> Result<Vec<IntegrityFinding>, GateError> {
    Err(GateError::Unimplemented {
        what: "integrity::check",
    })
}
