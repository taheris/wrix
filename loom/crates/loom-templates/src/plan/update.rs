use askama::Template;
use loom_driver::identifier::SpecLabel;

/// Context for `loom plan -u <label>` (update-spec interview).
#[derive(Template)]
#[template(path = "plan_update.md", escape = "none")]
pub struct PlanUpdateContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub companion_paths: Vec<String>,
    pub scratchpad_path: String,
    pub exit_signals: String,
}
