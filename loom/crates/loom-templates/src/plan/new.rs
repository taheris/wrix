use askama::Template;
use loom_core::identifier::SpecLabel;

/// Context for `loom plan -n <label>` (new-spec interview).
#[derive(Template)]
#[template(path = "plan_new.md", escape = "none")]
pub struct PlanNewContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub exit_signals: String,
}
