//! Integration tests that exercise every context struct's render path.
//!
//! Acceptance for `test_askama_templates_compile`, `test_template_partials`,
//! `test_template_output_parity`, and `test_template_compile_time_check`:
//! every template lives behind a typed context, partials resolve via
//! `{% include %}`, agent-supplied content is wrapped in `<agent-output>` and
//! `previous_failure` truncates at [`PREVIOUS_FAILURE_MAX_LEN`].

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use anyhow::Result;
use askama::Template;
use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};
use loom_templates::check::{CheckContext, ReviewSource};
use loom_templates::msg::{ClarifyBead, ClarifyOption, MsgContext};
use loom_templates::plan::{PlanNewContext, PlanUpdateContext};
use loom_templates::run::{PREVIOUS_FAILURE_MAX_LEN, PreviousFailure, RunContext};
use loom_templates::todo::{TodoNewContext, TodoUpdateContext};

const EXIT_SIGNALS_BODY: &str = "- `LOOM_COMPLETE`\n- `LOOM_BLOCKED`\n- `LOOM_CLARIFY`";
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
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
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
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Specification Update Interview"));
    assert!(out.contains("- lib/sandbox/"));
    assert!(out.contains("- lib/ralph/template/"));
    assert!(out.contains("Anchor Session & Sibling-Spec Editing"));
    assert!(out.contains("Invariant-Clash Awareness"));
    Ok(())
}

#[test]
fn todo_new_renders_spec_label_marker() -> Result<()> {
    let ctx = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Task Decomposition"));
    assert!(out.contains("spec:loom-harness"));
    // Implementation Notes section no longer rendered into todo prompts —
    // see D1 (wx-2ytty). The loom note CLI (D2) owns that surface.
    assert!(!out.contains("## Implementation Notes"));
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
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
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
        previous_failure: Some(PreviousFailure::new("error: cargo test failed".to_string())),
        scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-3hhwq.10/scratch.md".to_string(),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
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
fn previous_failure_truncates_at_max_len() {
    let huge = "x".repeat(PREVIOUS_FAILURE_MAX_LEN * 2);
    let pf = PreviousFailure::new(huge);
    assert!(pf.as_str().len() <= PREVIOUS_FAILURE_MAX_LEN);
}

#[test]
fn previous_failure_preserves_short_input() {
    let body = "boom".to_string();
    let pf = PreviousFailure::new(body.clone());
    assert_eq!(pf.as_str(), body);
}

#[test]
fn check_renders_review_context_fields() -> Result<()> {
    let verify_path = "tests/loom-test.sh";
    let verify_body = "test_review_inputs_include_judge_rubrics_signature() { :; }\n";
    let judge_path = "tests/judges/loom.sh";
    let judge_body = "judge_live_path_coverage_signature() { :; }\n";

    let ctx = CheckContext {
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
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
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

#[test]
fn msg_renders_clarify_beads_with_options() -> Result<()> {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
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
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Clarify Resolution — Drafter Session"));
    assert!(out.contains("### wx-clar.1 — [spec:loom-harness] State storage choice"));
    assert!(out.contains("## Options — State JSON vs. dedicated table"));
    assert!(out.contains("#### Option 1 — Keep state in JSON"));
    assert!(out.contains("Add a companions array."));
    assert!(out.contains("#### Option 2 — Migrate to a table"));
    assert!(out.contains("Use a SQLite table."));
    Ok(())
}

#[test]
fn msg_renders_with_no_clarify_beads() -> Result<()> {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        clarify_beads: vec![],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
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
        scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-mol.1/scratch.md".into(),
        exit_signals: "- `LOOM_COMPLETE`".into(),
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
