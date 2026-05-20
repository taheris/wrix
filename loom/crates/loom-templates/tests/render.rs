//! Integration tests that exercise every context struct's render path.
//!
//! Acceptance for `test_askama_templates_compile`, `test_template_partials`,
//! `test_template_output_parity`, and `test_template_compile_time_check`:
//! every template lives behind a typed context, partials resolve via
//! `{% include %}`, agent-supplied content is wrapped in `<agent-output>` and
//! `previous_failure` truncates at [`PREVIOUS_FAILURE_MAX_LEN`].

use anyhow::Result;
use askama::Template;
use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};
use loom_templates::msg::{ClarifyBead, ClarifyOption, MsgContext};
use loom_templates::plan::{PlanNewContext, PlanUpdateContext};
use loom_templates::review::{ReviewContext, ReviewSource};
use loom_templates::run::{
    DriverNoticeCause, PREVIOUS_FAILURE_MAX_LEN, PreviousFailure, ReviewConcernKind, RunContext,
    VerifierFailure,
};
use loom_templates::todo::{TodoNewContext, TodoUpdateContext};

const PINNED_CONTEXT_BODY: &str =
    "# Project Overview\n\nLoom orchestrates the spec-to-implementation workflow.";
const SCRATCHPAD_PATH_BODY: &str = "/workspace/.wrapix/loom/scratch/loom-harness/scratch.md";

#[test]
fn plan_new_renders_partials_and_inputs() -> Result<()> {
    let ctx = PlanNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Specification Interview"));
    assert!(out.contains(PINNED_CONTEXT_BODY));
    assert!(out.contains("Label: loom-harness"));
    assert!(out.contains("Spec file: specs/loom-harness.md"));
    assert!(out.contains("LOOM_COMPLETE"));
    assert!(out.contains("Interview Modes"));
    Ok(())
}

#[test]
fn plan_update_renders_partials_and_companions() -> Result<()> {
    let ctx = PlanUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into(), "lib/ralph/template/".into()],
        implementation_notes: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Specification Update Interview"));
    assert!(out.contains("- lib/sandbox/"));
    assert!(out.contains("- lib/ralph/template/"));
    assert!(out.contains("Anchor Session & Sibling-Spec Editing"));
    assert!(out.contains("Invariant-Clash Awareness"));
    Ok(())
}

/// Pins the three Plan-stage rubric checks (completeness, internal
/// coherence, invariant-clash) into both planning templates per
/// `specs/loom-gate.md` § Plan-stage checks. A future refactor must not
/// silently drop any of the three from the prompt.
#[test]
fn plan_templates_render_three_plan_stage_checks() -> Result<()> {
    let new_out = PlanNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    }
    .render()?;
    let update_out = PlanUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        implementation_notes: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    }
    .render()?;

    for (name, out) in [("plan_new", &new_out), ("plan_update", &update_out)] {
        assert!(
            out.contains("Plan-Stage Rubric"),
            "{name}: rubric heading missing"
        );
        assert!(
            out.contains("Completeness check"),
            "{name}: completeness check missing"
        );
        assert!(
            out.contains("Internal coherence check"),
            "{name}: internal coherence check missing"
        );
        assert!(
            out.contains("Invariant-clash scan") || out.contains("Invariant-Clash Awareness"),
            "{name}: invariant-clash check missing"
        );
        assert!(
            out.contains("three-paths"),
            "{name}: invariant-clash three-paths protocol not described"
        );
    }
    Ok(())
}

/// FR14 compliance: plan_new.md must not teach `[ ]` / `[x]` checkbox
/// syntax, must not instruct an `Affected Files` section, and must defer
/// the spec format to `docs/spec-conventions.md` rather than re-describing
/// it. Pins the audit findings (TA-1, TA-2, TA-7) so a refactor cannot
/// regress them.
#[test]
fn plan_new_defers_spec_format_to_conventions_doc() -> Result<()> {
    let out = PlanNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        spec_conventions: "docs/spec-conventions.md".to_string(),
    }
    .render()?;

    assert!(
        out.contains("docs/spec-conventions.md"),
        "plan_new must defer to docs/spec-conventions.md"
    );
    assert!(
        !out.contains("[ ] CLI") && !out.contains("[ ] Error"),
        "plan_new must not teach `[ ]` checkbox examples"
    );
    assert!(
        !out.contains("Affected files/modules") && !out.contains("Affected Files"),
        "plan_new must not instruct an Affected Files section"
    );
    Ok(())
}

#[test]
fn todo_new_renders_spec_label_marker() -> Result<()> {
    let ctx = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        implementation_notes: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Task Decomposition"));
    assert!(out.contains("spec:loom-harness"));
    // Empty notes → the section header is suppressed entirely (no empty
    // "## Implementation Notes" header in the prompt).
    assert!(!out.contains("## Implementation Notes"));
    Ok(())
}

#[test]
fn todo_new_renders_implementation_notes_when_present() -> Result<()> {
    let ctx = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        implementation_notes: vec![
            "Hidden constraint: touch lib/sandbox/linux/default.nix".into(),
            "Design trade-off: prefer single FK over join table".into(),
        ],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;
    assert!(out.contains("## Implementation Notes"));
    assert!(out.contains("Hidden constraint: touch lib/sandbox/linux/default.nix"));
    assert!(out.contains("Design trade-off: prefer single FK over join table"));
    assert_eq!(out.matches("<implementation-note>").count(), 2);
    assert_eq!(out.matches("</implementation-note>").count(), 2);
    Ok(())
}

#[test]
fn todo_update_renders_implementation_notes_when_present() -> Result<()> {
    let ctx = TodoUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        spec_diff: Some("=== specs/loom-harness.md ===\n+ change".into()),
        existing_tasks: None,
        molecule_id: Some(MoleculeId::new("wx-mol")),
        implementation_notes: vec!["beware FK cascade ordering".into()],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;
    assert!(out.contains("## Implementation Notes"));
    assert!(out.contains("beware FK cascade ordering"));
    assert!(out.contains("<implementation-note>"));
    Ok(())
}

#[test]
fn todo_update_wraps_existing_tasks_in_agent_output() -> Result<()> {
    let ctx = TodoUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        spec_diff: Some("=== specs/loom-harness.md ===\n+ new requirement".into()),
        existing_tasks: Some("- wx-3hhwq.1: scaffold workspace".into()),
        molecule_id: Some(MoleculeId::new("wx-3hhwq")),
        implementation_notes: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Add Tasks to Existing Molecule"));
    assert!(out.contains("=== specs/loom-harness.md ==="));
    assert!(out.contains("Molecule ID: wx-3hhwq"));
    let agent_open = out.find("<agent-output>");
    let agent_close = out.find("</agent-output>");
    assert!(agent_open.is_some() && agent_close.is_some());
    let (open, close) = (agent_open.unwrap_or(0), agent_close.unwrap_or(0));
    assert!(open < close);
    let inside = &out[open..close];
    assert!(inside.contains("wx-3hhwq.1: scaffold workspace"));
    Ok(())
}

#[test]
fn run_wraps_agent_supplied_fields_in_agent_output() -> Result<()> {
    let ctx = RunContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        molecule_id: Some(MoleculeId::new("wx-3hhwq")),
        issue_id: Some(BeadId::new("wx-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::from_agent_error(
            "error: cargo test failed",
        )),
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Implementation Step"));
    assert!(out.contains("Issue: wx-3hhwq.10"));
    assert!(out.contains("Title: <agent-output>port templates</agent-output>"));
    assert!(out.contains("Port templates to Askama."));
    assert!(out.contains("error: cargo test failed"));
    let count_open = out.matches("<agent-output>").count();
    let count_close = out.matches("</agent-output>").count();
    assert_eq!(count_open, count_close);
    assert!(
        count_open >= 3,
        "expected at least 3 agent-output blocks, got {count_open}"
    );
    Ok(())
}

#[test]
fn run_template_omits_attempt_line_when_zero() -> Result<()> {
    let ctx = RunContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("wx-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    assert!(
        !out.contains("Retry attempt"),
        "fresh dispatch must omit retry line: {out}",
    );
    Ok(())
}

#[test]
fn run_template_renders_attempt_line_on_retry() -> Result<()> {
    let ctx = RunContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("wx-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::ZeroProgress,
            detail: "Marker `LOOM_COMPLETE` emitted with empty diff.".into(),
        }),
        review_notes: None,
        attempt: 2,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    assert!(
        out.contains("Retry attempt 2 — previous attempt failed with:"),
        "retry line missing: {out}",
    );
    assert!(out.contains("Previous attempt: "), "framing missing: {out}");
    Ok(())
}

/// Per `specs/loom-harness.md` § Recovery context, the run prompt must
/// prepend a first-instruction reframe when `previous_failure.is_some() &&
/// attempt > 0`. Pins the canonical wording and ordering so a refactor
/// cannot silently drop or move the reframe.
#[test]
fn run_template_prepends_first_instruction_reframe_on_retry() -> Result<()> {
    let ctx = RunContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("wx-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::ZeroProgress,
            detail: "Marker `LOOM_COMPLETE` emitted with empty diff.".into(),
        }),
        review_notes: None,
        attempt: 1,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    let reframe = "> Re-read the previous failure block above and address its specific\n> concern before re-implementing.";
    assert!(out.contains(reframe), "reframe missing: {out}");
    let instructions_heading = out
        .find("## Instructions")
        .expect("## Instructions heading present");
    let reframe_pos = out.find(reframe).expect("reframe present");
    let first_step = out
        .find("1. **Understand**")
        .expect("first numbered step present");
    assert!(
        instructions_heading < reframe_pos && reframe_pos < first_step,
        "reframe must sit between the heading and step 1: heading={instructions_heading} reframe={reframe_pos} step1={first_step}",
    );
    Ok(())
}

/// Pins the false branch: fresh dispatch (`attempt = 0`, no
/// `previous_failure`) must not include the reframe blockquote — instruction
/// 1 follows the heading directly.
#[test]
fn run_template_omits_first_instruction_reframe_on_fresh_dispatch() -> Result<()> {
    let ctx = RunContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("wx-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    assert!(
        !out.contains("Re-read the previous failure block above"),
        "reframe must be absent on fresh dispatch: {out}",
    );
    Ok(())
}

/// Defensive boundary: `previous_failure.is_some()` alone is not enough —
/// the driver must also have bumped `attempt` past zero. If a caller wires
/// `attempt = 0` while supplying a `previous_failure`, the template stays in
/// the false branch (mirroring the existing `Retry attempt` line, which is
/// already gated on `attempt > 0`).
#[test]
fn run_template_omits_first_instruction_reframe_when_attempt_zero() -> Result<()> {
    let ctx = RunContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("wx-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::ZeroProgress,
            detail: "stray previous_failure with attempt=0".into(),
        }),
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    assert!(
        !out.contains("Re-read the previous failure block above"),
        "reframe must be absent when attempt is 0: {out}",
    );
    Ok(())
}

#[test]
fn run_template_renders_review_notes_block_when_set() -> Result<()> {
    let ctx = RunContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: Some(BeadId::new("wx-3hhwq.10")?),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::VerifyFailures(vec![VerifierFailure::new(
            "tests/a.sh",
            1,
            "boom\n",
        )])),
        review_notes: Some("[verifier-bypass] test mocks the agent backend".into()),
        attempt: 1,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-3hhwq.10/scratch.md".to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;
    assert!(out.contains("Review notes:"), "heading missing: {out}");
    assert!(
        out.contains("[verifier-bypass] test mocks the agent backend"),
        "review-notes body missing: {out}",
    );
    Ok(())
}

#[test]
fn previous_failure_truncates_at_max_len() {
    let huge = "x".repeat(PREVIOUS_FAILURE_MAX_LEN * 2);
    let pf = PreviousFailure::BuildFailure {
        stage: "cargo".into(),
        output: huge,
    };
    assert!(pf.to_string().len() <= PREVIOUS_FAILURE_MAX_LEN);
}

#[test]
fn previous_failure_renders_review_concern_with_token() {
    let pf = PreviousFailure::ReviewConcern {
        concern: ReviewConcernKind::MockDiscipline,
        reason: "mock is the thing under test".into(),
    };
    let rendered = pf.to_string();
    assert!(rendered.contains("(mock-discipline)"), "{rendered}");
    assert!(
        rendered.contains("mock is the thing under test"),
        "{rendered}"
    );
}

#[test]
fn review_renders_review_context_fields() -> Result<()> {
    let verify_path = "tests/loom/run-tests.sh";
    let verify_body = "test_review_inputs_include_judge_rubrics_signature() { :; }\n";
    let judge_path = "tests/judges/loom.sh";
    let judge_body = "judge_live_path_coverage_signature() { :; }\n";

    let ctx = ReviewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        beads_summary: Some("- wx-3hhwq.10: closed".into()),
        base_commit: Some("abc1234".into()),
        molecule_id: Some(MoleculeId::new("wx-3hhwq")),
        verify_sources: vec![ReviewSource {
            path: verify_path.into(),
            body: verify_body.into(),
        }],
        judge_rubrics: vec![ReviewSource {
            path: judge_path.into(),
            body: judge_body.into(),
        }],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Post-Epic Review"));
    assert!(out.contains("Base commit**: abc1234"));
    assert!(out.contains("Molecule**: wx-3hhwq"));
    assert!(out.contains("git diff abc1234..HEAD"));
    assert!(out.contains("- wx-3hhwq.10: closed"));

    assert!(out.contains("## `[verify]` Sources"));
    assert!(out.contains(verify_path), "verify path missing: {out}");
    assert!(
        out.contains(verify_body.trim()),
        "verify body missing: {out}"
    );

    assert!(out.contains("## `[judge]` Rubrics"));
    assert!(out.contains(judge_path), "judge path missing: {out}");
    assert!(out.contains(judge_body.trim()), "judge body missing: {out}");
    Ok(())
}

/// The review rubric must walk `{{ style_rules }}` rule by rule and require
/// rule-id + file/line citations. Per `specs/loom-templates.md` *Style-Rules
/// Partial*, the rubric is **rule-family-agnostic**: it tells the judge to
/// discover families from the pinned document rather than enumerate them in
/// the prompt. This test pins those directives so a future refactor cannot
/// silently drop them.
#[test]
fn review_renders_style_rule_conformance_walkthrough() -> Result<()> {
    let ctx = ReviewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        beads_summary: None,
        base_commit: None,
        molecule_id: None,
        verify_sources: vec![],
        judge_rubrics: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("## Style-Rule Conformance"),
        "rubric section heading missing: {out}",
    );
    assert!(
        out.contains("docs/style-rules.md"),
        "style_rules path not pinned: {out}",
    );
    assert!(
        out.contains("Discover the families") && out.contains("do not assume a fixed prefix list"),
        "family-discovery instruction missing: {out}",
    );
    assert!(
        out.contains("rule id"),
        "citation contract (rule id) not described: {out}",
    );
    assert!(
        out.contains("file and line range") || out.contains("file/line range"),
        "citation contract (file/line range) not described: {out}",
    );
    assert!(
        out.contains("LOOM_CONCERN: style-rule"),
        "style-rule concern marker not documented: {out}",
    );
    assert!(
        out.contains("`style-rule`"),
        "style-rule concern token missing from flag schema: {out}",
    );
    for forbidden in ["**SH-**", "**NX-**", "**RS-**", "**COM-**", "**CLI-**"] {
        assert!(
            !out.contains(forbidden),
            "rule-family marker {forbidden} leaked into prompt: {out}",
        );
    }
    Ok(())
}

/// Pins the Options Format Contract's universal scope and explicit
/// `bd update --notes` + `loom:clarify` flow for existing beads, so the
/// contract cannot silently re-narrow to invariant-clash-only.
#[test]
fn review_renders_options_format_contract_with_universal_scope() -> Result<()> {
    let ctx = ReviewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        beads_summary: None,
        base_commit: None,
        molecule_id: None,
        verify_sources: vec![],
        judge_rubrics: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    };
    let out = ctx.render()?;

    assert!(
        out.contains("Options Format Contract"),
        "Options Format Contract section missing: {out}",
    );
    assert!(
        out.contains("scope is universal") || out.contains("clarify situation"),
        "contract scope not generalised beyond invariant clashes: {out}",
    );
    assert!(
        out.contains("bd update")
            && out.contains("--notes")
            && out.contains("--add-label=loom:clarify"),
        "bd update --notes + loom:clarify flow for EXISTING beads not documented: {out}",
    );
    assert!(
        out.contains("gate does NOT parse your prose")
            || out.contains("does not scrape")
            || out.contains("does NOT parse your prose"),
        "persistence-boundary statement missing: {out}",
    );
    assert!(
        out.contains("## Options —") && out.contains("### Option 1 —"),
        "canonical Options block shape missing: {out}",
    );
    Ok(())
}

#[test]
fn msg_renders_clarify_beads_with_options() -> Result<()> {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        clarify_beads: vec![ClarifyBead {
            id: BeadId::new("wx-clar.1")?,
            spec_label: SpecLabel::new("loom-harness"),
            title: "State storage choice".into(),
            options_summary: Some("State JSON vs. dedicated table".into()),
            options: vec![
                ClarifyOption {
                    n: 1,
                    title: Some("Keep state in JSON".into()),
                    body: Some("Add a companions array.".into()),
                },
                ClarifyOption {
                    n: 2,
                    title: Some("Migrate to a table".into()),
                    body: Some("Use a SQLite table.".into()),
                },
            ],
        }],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Clarify Resolution — Drafter Session"));
    assert!(out.contains("### wx-clar.1 — [spec:loom-harness] State storage choice"));
    assert!(out.contains("## Options — State JSON vs. dedicated table"));
    assert!(out.contains("#### Option 1 — Keep state in JSON"));
    assert!(out.contains("Add a companions array."));
    assert!(out.contains("#### Option 2 — Migrate to a table"));
    assert!(out.contains("Use a SQLite table."));
    assert!(out.contains("## Companions"));
    assert!(out.contains("- lib/sandbox/"));
    Ok(())
}

#[test]
fn msg_renders_with_no_clarify_beads() -> Result<()> {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        companion_paths: vec![],
        clarify_beads: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Clarify Resolution — Drafter Session"));
    assert!(!out.contains("### wx-"));
    Ok(())
}

/// Smoke check: the rendered run prompt contains every instruction section,
/// header, and substituted value the run.md template promises for shared
/// inputs.
#[test]
fn run_renders_expected_sections_for_shared_inputs() -> Result<()> {
    let ctx = RunContext {
        pinned_context: "PIN".into(),
        label: SpecLabel::new("demo"),
        spec_path: "specs/demo.md".into(),
        companion_paths: vec!["lib/demo/".into()],
        molecule_id: Some(MoleculeId::new("wx-mol")),
        issue_id: Some(BeadId::new("wx-mol.1")?),
        title: Some("the title".into()),
        description: Some("the description".into()),
        previous_failure: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-mol.1/scratch.md".into(),
        style_rules: "docs/style-rules.md".into(),
    };
    let out = ctx.render()?;

    for shared in [
        "## Context Pinning",
        "## Current Feature",
        "## Companions",
        "## Issue Details",
        "Issue: wx-mol.1",
        "the title",
        "the description",
        "`bd ready`",
        "## Spec Verifications",
        "## Quality Gates",
        "## Land the Plane",
        "## Exit Signals",
    ] {
        assert!(
            out.contains(shared),
            "loom run missing shared section: {shared}"
        );
    }
    Ok(())
}

/// Returns true when `needle` falls inside any `<agent-output>...</agent-output>`
/// span in `haystack`. Used to assert that each agent-supplied field is
/// delimited by the markers, not merely that the markers appear somewhere in
/// the rendered prompt.
fn contained_within_agent_output(haystack: &str, needle: &str) -> bool {
    const OPEN: &str = "<agent-output>";
    const CLOSE: &str = "</agent-output>";
    let mut cursor = 0;
    while let Some(open_rel) = haystack[cursor..].find(OPEN) {
        let span_start = cursor + open_rel + OPEN.len();
        let Some(close_rel) = haystack[span_start..].find(CLOSE) else {
            return false;
        };
        let span_end = span_start + close_rel;
        if haystack[span_start..span_end].contains(needle) {
            return true;
        }
        cursor = span_end + CLOSE.len();
    }
    false
}

/// Pins the per-field agent-output wrapping: each of the four agent-supplied
/// fields (`title`, `description`, `previous_failure`, `existing_tasks`) is
/// rendered inside an `<agent-output>` span, not merely in the same prompt.
#[test]
fn agent_output_markers_wrap_each_agent_supplied_field() -> Result<()> {
    let run = RunContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        molecule_id: Some(MoleculeId::new("wx-3hhwq")),
        issue_id: Some(BeadId::new("wx-3hhwq.10")?),
        title: Some("AGENTOUT_TITLE_TOKEN".into()),
        description: Some("AGENTOUT_DESC_TOKEN".into()),
        previous_failure: Some(PreviousFailure::from_agent_error("AGENTOUT_FAILURE_TOKEN")),
        review_notes: None,
        attempt: 1,
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        style_rules: "docs/style-rules.md".to_string(),
    }
    .render()?;
    for token in [
        "AGENTOUT_TITLE_TOKEN",
        "AGENTOUT_DESC_TOKEN",
        "AGENTOUT_FAILURE_TOKEN",
    ] {
        assert!(
            contained_within_agent_output(&run, token),
            "run.md: {token} not enclosed in <agent-output>: {run}",
        );
    }

    let todo_update = TodoUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        spec_diff: Some("=== specs/loom-harness.md ===\n+ change".into()),
        existing_tasks: Some("AGENTOUT_TASKS_TOKEN".into()),
        molecule_id: Some(MoleculeId::new("wx-3hhwq")),
        implementation_notes: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
    }
    .render()?;
    assert!(
        contained_within_agent_output(&todo_update, "AGENTOUT_TASKS_TOKEN"),
        "todo_update.md: existing_tasks not enclosed in <agent-output>: {todo_update}",
    );
    Ok(())
}

/// Pins template-render determinism: every context renders byte-identically
/// twice in a row from identical inputs. Catches non-determinism (HashMap
/// ordering, time, env reads) that snapshots would only flag on the next
/// snapshot review.
#[test]
fn template_renders_are_byte_stable_across_runs() -> Result<()> {
    fn assert_stable<T: Template>(name: &str, ctx: T) -> Result<()> {
        let first = ctx.render()?;
        let second = ctx.render()?;
        assert_eq!(
            first, second,
            "{name}: render output differs between two consecutive renders with identical inputs",
        );
        Ok(())
    }

    assert_stable(
        "plan_new",
        PlanNewContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("loom-harness"),
            spec_path: "specs/loom-harness.md".to_string(),
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
            spec_conventions: "docs/spec-conventions.md".to_string(),
        },
    )?;
    assert_stable(
        "plan_update",
        PlanUpdateContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("loom-harness"),
            spec_path: "specs/loom-harness.md".to_string(),
            companion_paths: vec!["lib/sandbox/".into()],
            implementation_notes: vec!["pin: stability check".into()],
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
            spec_conventions: "docs/spec-conventions.md".to_string(),
        },
    )?;
    assert_stable(
        "todo_new",
        TodoNewContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("loom-harness"),
            spec_path: "specs/loom-harness.md".to_string(),
            companion_paths: vec!["lib/sandbox/".into()],
            implementation_notes: vec![],
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        },
    )?;
    assert_stable(
        "todo_update",
        TodoUpdateContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("loom-harness"),
            spec_path: "specs/loom-harness.md".to_string(),
            companion_paths: vec![],
            spec_diff: Some("=== specs/loom-harness.md ===\n+ stability".into()),
            existing_tasks: Some("- wx-3hhwq.1: scaffold".into()),
            molecule_id: Some(MoleculeId::new("wx-3hhwq")),
            implementation_notes: vec![],
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        },
    )?;
    assert_stable(
        "run",
        RunContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("loom-harness"),
            spec_path: "specs/loom-harness.md".to_string(),
            companion_paths: vec!["lib/sandbox/".into()],
            molecule_id: Some(MoleculeId::new("wx-3hhwq")),
            issue_id: Some(BeadId::new("wx-3hhwq.10")?),
            title: Some("port templates".into()),
            description: Some("Port templates to Askama.".into()),
            previous_failure: Some(PreviousFailure::from_agent_error(
                "error: cargo test failed",
            )),
            review_notes: None,
            attempt: 1,
            scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-3hhwq.10/scratch.md".to_string(),
            style_rules: "docs/style-rules.md".to_string(),
        },
    )?;
    assert_stable(
        "review",
        ReviewContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            label: SpecLabel::new("loom-harness"),
            spec_path: "specs/loom-harness.md".to_string(),
            companion_paths: vec!["lib/sandbox/".into()],
            beads_summary: Some("- wx-3hhwq.10: closed".into()),
            base_commit: Some("abc1234".into()),
            molecule_id: Some(MoleculeId::new("wx-3hhwq")),
            verify_sources: vec![ReviewSource {
                path: "tests/loom/run-tests.sh".into(),
                body: "test_review_inputs() { :; }\n".into(),
            }],
            judge_rubrics: vec![ReviewSource {
                path: "tests/judges/loom.sh".into(),
                body: "judge_live_path_coverage() { :; }\n".into(),
            }],
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
            style_rules: "docs/style-rules.md".to_string(),
        },
    )?;
    assert_stable(
        "msg",
        MsgContext {
            pinned_context: PINNED_CONTEXT_BODY.to_string(),
            companion_paths: vec!["lib/sandbox/".into()],
            clarify_beads: vec![ClarifyBead {
                id: BeadId::new("wx-clar.1")?,
                spec_label: SpecLabel::new("loom-harness"),
                title: "State storage choice".into(),
                options_summary: Some("State JSON vs. dedicated table".into()),
                options: vec![
                    ClarifyOption {
                        n: 1,
                        title: Some("Keep state in JSON".into()),
                        body: Some("Add a companions array.".into()),
                    },
                    ClarifyOption {
                        n: 2,
                        title: Some("Migrate to a table".into()),
                        body: Some("Use a SQLite table.".into()),
                    },
                ],
            }],
            scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        },
    )?;
    Ok(())
}
