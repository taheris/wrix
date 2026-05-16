//! `loom check` — deterministic spec-vs-implementation audits.
//!
//! Each sub-audit lives in its own submodule. `criteria` implements
//! `loom check --check=criteria` (FR14): the spec ↔ test-dispatcher walk
//! that flags stubbed, missing, masqueraded, and orphan verifiers.
//! Future sub-audits (`surface`, `removals`, `infrastructure`,
//! `cross-spec`) sit beside it.

pub mod criteria;
