//! `loom check` template: the post-epic reviewer prompt.

use askama::Template;
use loom_core::identifier::{MoleculeId, SpecLabel};

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
    pub exit_signals: String,
}
