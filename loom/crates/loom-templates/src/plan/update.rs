use askama::Template;
use loom_core::identifier::SpecLabel;

/// Context for `loom plan -u <label>` (update-spec interview).
#[derive(Template)]
#[template(path = "plan_update.md", escape = "none")]
pub struct PlanUpdateContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub companion_paths: Vec<String>,
    /// Notes already on `specs.implementation_notes` for `label`. The
    /// template renders them so the interview can perform the merge
    /// described in `specs/loom-harness.md` § Implementation-notes
    /// lifecycle. Empty in tier-fresh paths or when notes were just
    /// consumed by `loom todo`.
    pub existing_implementation_notes: Vec<String>,
    pub exit_signals: String,
}
