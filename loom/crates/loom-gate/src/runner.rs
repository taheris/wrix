//! Runner discovery for batched tiers (`[test]`, `[judge]`).
//!
//! Two layered mechanisms per `specs/loom-gate.md`: toolchain-detection
//! defaults (Cargo.toml → nextest, pyproject.toml → pytest, go.mod →
//! `go test`) and a `.loom/config.toml` override path for repos where the
//! defaults do not fit. The actual detection and TOML loading live in the
//! runner-discovery bead — this scaffold exposes the [`RunnerTemplate`]
//! shape plus a stub entry point.

use std::path::Path;

use crate::GateError;
use crate::annotation::Tier;

/// Template string for a batched-tier runner with a `{paths}` placeholder
/// substituted at invocation time. Default templates come from toolchain
/// detection; an opt-in `.loom/config.toml` overrides per tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerTemplate {
    pub command: String,
}

/// Resolve the runner template for `tier` rooted at `repo_root`. Stub —
/// implemented in the runner-discovery bead.
pub fn discover(_repo_root: &Path, _tier: Tier) -> Result<RunnerTemplate, GateError> {
    Err(GateError::Unimplemented {
        what: "runner::discover",
    })
}
