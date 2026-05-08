use askama::Template;

use loom_templates::plan::{PlanNewContext, PlanUpdateContext};

use super::args::PlanMode;
use super::error::PlanError;

/// Inputs threaded into the plan-new/plan-update context structs.
///
/// Constructed once by the driver per `loom plan` call. `companion_paths`
/// is consumed only by `Update`; `New` ignores it — companions are an
/// update-only concept.
pub struct PlanPromptInputs {
    pub mode: PlanMode,
    pub spec_path: String,
    pub pinned_context: String,
    pub companion_paths: Vec<String>,
    /// Existing notes pulled from `specs.implementation_notes` for the
    /// `Update` mode so the agent can perform the merge required by
    /// `specs/loom-harness.md` § Implementation-notes lifecycle. Ignored for
    /// `New`, where the row does not yet exist.
    pub existing_implementation_notes: Vec<String>,
    /// Absolute path to `.wrapix/loom/scratch/<key>/scratch.md` for this
    /// session. Embedded in the rendered prompt so the agent can write to
    /// the correct file under compaction recovery.
    pub scratchpad_path: String,
    pub exit_signals: String,
}

/// Render the appropriate Askama template for `inputs.mode`. Returns the
/// rendered prompt body the driver will pass to `wrapix run`.
pub fn render_prompt(inputs: PlanPromptInputs) -> Result<String, PlanError> {
    let body = match inputs.mode {
        PlanMode::New(label) => PlanNewContext {
            pinned_context: inputs.pinned_context,
            label,
            spec_path: inputs.spec_path,
            scratchpad_path: inputs.scratchpad_path,
            exit_signals: inputs.exit_signals,
        }
        .render()?,
        PlanMode::Update(label) => PlanUpdateContext {
            pinned_context: inputs.pinned_context,
            label,
            spec_path: inputs.spec_path,
            companion_paths: inputs.companion_paths,
            existing_implementation_notes: inputs.existing_implementation_notes,
            scratchpad_path: inputs.scratchpad_path,
            exit_signals: inputs.exit_signals,
        }
        .render()?,
    };
    Ok(body)
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use loom_core::identifier::SpecLabel;

    fn inputs_new() -> PlanPromptInputs {
        PlanPromptInputs {
            mode: PlanMode::New(SpecLabel::new("loom-harness")),
            spec_path: "specs/loom-harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec![],
            existing_implementation_notes: vec![],
            scratchpad_path: "/workspace/.wrapix/loom/scratch/loom-harness/scratch.md".into(),
            exit_signals: "LOOM_COMPLETE".into(),
        }
    }

    fn inputs_update() -> PlanPromptInputs {
        PlanPromptInputs {
            mode: PlanMode::Update(SpecLabel::new("loom-harness")),
            spec_path: "specs/loom-harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec!["lib/sandbox/".into()],
            existing_implementation_notes: vec!["touch lib/foo".into()],
            scratchpad_path: "/workspace/.wrapix/loom/scratch/loom-harness/scratch.md".into(),
            exit_signals: "LOOM_COMPLETE".into(),
        }
    }

    #[test]
    fn new_renders_specification_interview_header() {
        let body = render_prompt(inputs_new()).expect("render");
        assert!(body.contains("# Specification Interview"));
        assert!(body.contains("specs/loom-harness.md"));
        assert!(body.contains("LOOM_COMPLETE"));
    }

    #[test]
    fn update_renders_specification_update_header_with_companion() {
        let body = render_prompt(inputs_update()).expect("render");
        assert!(body.contains("# Specification Update Interview"));
        assert!(body.contains("- lib/sandbox/"));
    }

    #[test]
    fn update_renders_existing_implementation_notes_for_merge() {
        let body = render_prompt(inputs_update()).expect("render");
        assert!(
            body.contains("touch lib/foo"),
            "existing notes must be threaded into the prompt so the agent can merge",
        );
    }
}
