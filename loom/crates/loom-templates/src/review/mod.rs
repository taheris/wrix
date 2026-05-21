//! `loom review` template: the LLM-judged rubric prompt.

use askama::Template;
use loom_events::identifier::{MoleculeId, SpecLabel};

/// One file body included in the review prompt — either a `[verify]` test
/// script the gate just ran, or a `[judge]` rubric the LLM must score
/// against. `path` is the workspace-relative source location used as the
/// rendered section title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSource {
    pub path: String,
    pub body: String,
}

/// Context for `loom review` reviewing a completed molecule.
#[derive(Template)]
#[template(path = "review.md", escape = "none")]
pub struct ReviewContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub companion_paths: Vec<String>,
    pub beads_summary: Option<String>,
    pub base_commit: Option<String>,
    pub molecule_id: Option<MoleculeId>,
    pub verify_sources: Vec<ReviewSource>,
    pub judge_rubrics: Vec<ReviewSource>,
    pub scratchpad_path: String,
    /// Workspace-relative path to the project's style-rules document
    /// (`docs/style-rules.md` by default). Pinned in the review prompt so
    /// the LLM walks the rules rule-by-rule and cites each violation by
    /// rule id + file/line.
    pub style_rules: String,
}
