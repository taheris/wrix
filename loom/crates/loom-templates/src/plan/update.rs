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
    /// Current `kind = implementation` notes for this spec, fetched by the
    /// runner via `notes_list(label, "implementation")` before launching the
    /// interview. Threaded into the prompt so the agent can perform the
    /// keep/drop/add merge described in `specs/loom-harness.md`
    /// § Implementation-notes lifecycle and write the merged array back via
    /// `loom note set <label> --kind implementation --json '[…]'`.
    pub implementation_notes: Vec<String>,
    pub scratchpad_path: String,
    pub exit_signals: String,
}
