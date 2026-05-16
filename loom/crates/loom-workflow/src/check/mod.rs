//! `loom check` — deterministic spec-vs-implementation audits.
//!
//! Each sub-audit lives in its own submodule. `criteria` implements
//! `loom check criteria` (FR14): the spec ↔ test-dispatcher walk that
//! flags stubbed, missing, masqueraded, and orphan verifiers. `matrix`
//! audits the pinning matrix in `specs/loom-templates.md` against the
//! actual `{% include %}` graph in `loom-templates/templates/`. Future
//! sub-audits (`surface`, FR13) sit beside them.

pub mod criteria;
pub mod matrix;
