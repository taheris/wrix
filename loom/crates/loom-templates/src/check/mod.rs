//! `loom check` template: the post-epic reviewer prompt.

use askama::Template;
use loom_core::identifier::{MoleculeId, SpecLabel};

/// One file body included in the review prompt — either a `[verify]` test
/// script the gate just ran, or a `[judge]` rubric the LLM must score
/// against. `path` is the workspace-relative source location used as the
/// rendered section title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSource {
    pub path: String,
    pub body: String,
}

/// Context for `loom check` reviewing a completed molecule.
#[derive(Template)]
#[template(path = "check.md", escape = "none")]
pub struct CheckContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub companion_paths: Vec<String>,
    pub beads_summary: Option<String>,
    pub base_commit: Option<String>,
    pub molecule_id: Option<MoleculeId>,
    pub verify_sources: Vec<ReviewSource>,
    pub judge_rubrics: Vec<ReviewSource>,
    pub exit_signals: String,
}
