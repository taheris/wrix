use loom_core::identifier::{BeadId, MoleculeId, SpecLabel};
use loom_templates::run::{PreviousFailure, RunContext};

/// Inputs for [`build_run_context`]. Constructed once per bead spawn — for
/// retries the driver rebuilds with `previous_failure` set.
pub struct RunContextInputs {
    pub label: SpecLabel,
    pub spec_path: String,
    pub pinned_context: String,
    pub companion_paths: Vec<String>,
    pub molecule_id: Option<MoleculeId>,
    pub issue_id: BeadId,
    pub title: String,
    pub description: String,
    /// Raw failure body from the previous attempt. Truncated to 4000 chars
    /// inside [`PreviousFailure::new`] when wrapped — see `templates/run.md`.
    pub previous_failure: Option<String>,
    pub exit_signals: String,
}

/// Build the typed [`RunContext`] for a single bead spawn from the driver's
/// per-iteration inputs.
pub fn build_run_context(inputs: RunContextInputs) -> RunContext {
    RunContext {
        pinned_context: inputs.pinned_context,
        label: inputs.label,
        spec_path: inputs.spec_path,
        companion_paths: inputs.companion_paths,
        molecule_id: inputs.molecule_id,
        issue_id: Some(inputs.issue_id),
        title: Some(inputs.title),
        description: Some(inputs.description),
        previous_failure: inputs.previous_failure.map(PreviousFailure::new),
        exit_signals: inputs.exit_signals,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use askama::Template;

    fn inputs() -> RunContextInputs {
        RunContextInputs {
            label: SpecLabel::new("loom-harness"),
            spec_path: "specs/loom-harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec![],
            molecule_id: Some(MoleculeId::new("wx-3hhwq")),
            issue_id: BeadId::new("wx-3hhwq.15").expect("valid bead id"),
            title: "Implement loom run".into(),
            description: "Per-bead loop".into(),
            previous_failure: None,
            exit_signals: "LOOM_COMPLETE".into(),
        }
    }

    #[test]
    fn retry_input_wraps_previous_failure() {
        let mut i = inputs();
        i.previous_failure = Some("cargo test failed".into());
        let ctx = build_run_context(i);
        let pf = ctx.previous_failure.expect("set on retry");
        assert_eq!(pf.as_str(), "cargo test failed");
    }

    #[test]
    fn first_attempt_omits_previous_failure() {
        let ctx = build_run_context(inputs());
        assert!(ctx.previous_failure.is_none());
    }

    #[test]
    fn rendered_prompt_includes_issue_and_title() {
        let ctx = build_run_context(inputs());
        let body = ctx.render().expect("render");
        assert!(body.contains("wx-3hhwq.15"), "{body}");
        assert!(body.contains("Implement loom run"), "{body}");
    }

    #[test]
    fn rendered_retry_prompt_includes_previous_failure_body() {
        let mut i = inputs();
        i.previous_failure = Some("STDERR: cargo test failure".into());
        let ctx = build_run_context(i);
        let body = ctx.render().expect("render");
        assert!(body.contains("STDERR: cargo test failure"), "{body}");
    }
}
