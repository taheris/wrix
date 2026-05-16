//! `loom check` — deterministic spec-vs-implementation audits.
//!
//! Each sub-audit lives in its own submodule. `criteria` implements
//! `loom check criteria` (FR14): the spec ↔ test-dispatcher walk that
//! flags stubbed, missing, masqueraded, and orphan verifiers. `matrix`
//! audits the pinning matrix in `specs/loom-templates.md` against the
//! actual `{% include %}` graph in `loom-templates/templates/`.
//! `surface` (FR13) audits the binary's user-facing surface (commands,
//! flags, removed surface, grouping order) against `specs/loom-harness.md`.

pub mod criteria;
pub mod matrix;
pub mod surface;
