use askama::Template;
use loom_core::identifier::SpecLabel;

/// Context for `loom todo` decomposing a fresh spec into a new molecule.
#[derive(Template)]
#[template(path = "todo_new.md", escape = "none")]
pub struct TodoNewContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub companion_paths: Vec<String>,
    pub implementation_notes: Vec<String>,
    pub exit_signals: String,
}
