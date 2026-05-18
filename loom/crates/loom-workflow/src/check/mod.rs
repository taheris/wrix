//! `loom check` — deterministic spec-vs-implementation audits.
//!
//! Each sub-audit lives in its own submodule. `criteria` implements
//! `loom check criteria` (FR14): the spec ↔ test-dispatcher walk that
//! flags stubbed, missing, and orphan verifiers. `matrix` audits the
//! pinning matrix in `specs/loom-templates.md` against the actual
//! `{% include %}` graph in `loom-templates/templates/`. `surface`
//! (FR13) audits the binary's user-facing surface (commands, flags,
//! removed surface, grouping order) against `specs/loom-harness.md`.

pub mod cargo_dispatch;
pub mod criteria;
pub mod matrix;
pub mod surface;

use std::collections::HashSet;
use std::path::PathBuf;

use loom_driver::identifier::BeadId;

/// Audit scope. Per FR1, scope flags compose with both bare `loom
/// check` and the positional `loom check <audit>` form.
///
/// `Default` and `Tree` are observably identical for the audits that
/// exist today (criteria walks every spec by default; surface and
/// matrix have no scope dimension). Tree exists as a distinct variant
/// because the molecule-completion handoff (`loom run` → `loom check
/// --tree`) advertises whole-tree intent at the call site, and a
/// future audit may consult it.
#[derive(Debug, Clone)]
pub enum Scope {
    /// No narrowing — each audit runs at its natural default scope.
    Default,
    /// Narrow to one bead (criteria narrows to that bead's spec; surface
    /// and matrix are bead-independent and ignore this).
    Bead(BeadId),
    /// Narrow to a git diff range (criteria narrows to specs touched by
    /// the diff; surface and matrix are diff-independent).
    Diff(String),
    /// Walk every spec × every implementation file.
    Tree,
}

impl Scope {
    /// True when the scope is a per-bead or per-diff narrowing — a hint
    /// that bead/diff-independent audits (surface, matrix) should
    /// announce that they ignored the scope rather than silently re-run
    /// the whole audit. Today they still run the same audit; this flag
    /// drives a one-line note in the aggregate report.
    pub fn is_narrow(&self) -> bool {
        matches!(self, Scope::Bead(_) | Scope::Diff(_))
    }
}

/// Resolved spec-file filter for the criteria audit. The driver
/// translates a [`Scope`] to one of these via bd / git lookups before
/// calling [`criteria::audit_filtered`].
#[derive(Debug, Clone, Default)]
pub struct CriteriaScope {
    /// `None` ↔ no filter (Default / Tree). `Some(set)` ↔ only consider
    /// annotations whose spec file is in the set.
    pub spec_files: Option<HashSet<PathBuf>>,
}

impl CriteriaScope {
    pub fn unfiltered() -> Self {
        Self { spec_files: None }
    }

    pub fn from_files<I: IntoIterator<Item = PathBuf>>(files: I) -> Self {
        Self {
            spec_files: Some(files.into_iter().collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_narrow_only_for_bead_and_diff() {
        let bead = BeadId::new("wx-3hhwq.53").expect("bead id");
        assert!(!Scope::Default.is_narrow());
        assert!(!Scope::Tree.is_narrow());
        assert!(Scope::Bead(bead).is_narrow());
        assert!(Scope::Diff("HEAD~1..HEAD".to_string()).is_narrow());
    }

    #[test]
    fn criteria_scope_unfiltered_has_no_spec_set() {
        assert!(CriteriaScope::unfiltered().spec_files.is_none());
    }

    #[test]
    fn criteria_scope_from_files_collects_unique_paths() {
        let dup = PathBuf::from("specs/a.md");
        let scope = CriteriaScope::from_files([dup.clone(), dup, PathBuf::from("specs/b.md")]);
        let set = scope.spec_files.expect("filter set");
        assert_eq!(set.len(), 2);
        assert!(set.contains(&PathBuf::from("specs/a.md")));
        assert!(set.contains(&PathBuf::from("specs/b.md")));
    }
}
