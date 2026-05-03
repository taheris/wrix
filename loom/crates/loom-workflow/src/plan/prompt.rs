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
            exit_signals: inputs.exit_signals,
        }
        .render()?,
        PlanMode::Update(label) => PlanUpdateContext {
            pinned_context: inputs.pinned_context,
            label,
            spec_path: inputs.spec_path,
            companion_paths: inputs.companion_paths,
            exit_signals: inputs.exit_signals,
        }
        .render()?,
    };
    Ok(body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use loom_core::identifier::SpecLabel;

    fn inputs_new() -> PlanPromptInputs {
        PlanPromptInputs {
            mode: PlanMode::New(SpecLabel::new("loom-harness")),
            spec_path: "specs/loom-harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec![],
            exit_signals: "LOOM_COMPLETE".into(),
        }
    }

    fn inputs_update() -> PlanPromptInputs {
        PlanPromptInputs {
            mode: PlanMode::Update(SpecLabel::new("loom-harness")),
            spec_path: "specs/loom-harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec!["lib/sandbox/".into()],
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
}
