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
use loom_core::identifier::{BeadId, MoleculeId, SpecLabel};
use loom_templates::check::CheckContext;
use loom_templates::msg::{ClarifyBead, ClarifyOption, MsgContext};
use loom_templates::plan::{PlanNewContext, PlanUpdateContext};
use loom_templates::run::{PREVIOUS_FAILURE_MAX_LEN, PreviousFailure, RunContext};
use loom_templates::todo::{TodoNewContext, TodoUpdateContext};

const EXIT_SIGNALS_BODY: &str = "- `LOOM_COMPLETE`\n- `LOOM_BLOCKED`\n- `LOOM_CLARIFY`";
const PINNED_CONTEXT_BODY: &str =
    "# Project Overview\n\nLoom orchestrates the spec-to-implementation workflow.";

#[test]
fn plan_new_renders_partials_and_inputs() -> Result<()> {
    let ctx = PlanNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Specification Interview"));
    assert!(out.contains(PINNED_CONTEXT_BODY));
    assert!(out.contains("Label: loom-harness"));
    assert!(out.contains("Spec file: specs/loom-harness.md"));
    assert!(out.contains("LOOM_COMPLETE"));
    assert!(out.contains("Implementation Notes Section"));
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
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Specification Update Interview"));
    assert!(out.contains("- lib/sandbox/"));
    assert!(out.contains("- lib/ralph/template/"));
    assert!(out.contains("Anchor Session & Sibling-Spec Editing"));
    assert!(out.contains("Invariant-Clash Awareness"));
    assert!(out.contains("Implementation Notes"));
    Ok(())
}

#[test]
fn todo_new_renders_implementation_notes_when_present() -> Result<()> {
    let ctx = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        implementation_notes: vec![
            "Remove rustup bootstrap block".to_string(),
            "Use fenix fromToolchainFile".to_string(),
        ],
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Task Decomposition"));
    assert!(out.contains("## Implementation Notes"));
    assert!(out.contains("- Remove rustup bootstrap block"));
    assert!(out.contains("- Use fenix fromToolchainFile"));
    assert!(out.contains("spec:loom-harness"));
    Ok(())
}

#[test]
fn todo_new_omits_implementation_notes_section_when_empty() -> Result<()> {
    let ctx = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        implementation_notes: vec![],
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(!out.contains("## Implementation Notes\n\n-"));
    Ok(())
}

#[test]
fn todo_update_wraps_existing_tasks_in_agent_output() -> Result<()> {
    let ctx = TodoUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec![],
        implementation_notes: vec![],
        spec_diff: Some("=== specs/loom-harness.md ===\n+ new requirement".into()),
        existing_tasks: Some("- wx-3hhwq.1: scaffold workspace".into()),
        molecule_id: Some(MoleculeId::new("wx-3hhwq")),
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
    let ctx = CheckContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        beads_summary: Some("- wx-3hhwq.10: closed".into()),
        base_commit: Some("abc1234".into()),
        molecule_id: Some(MoleculeId::new("wx-3hhwq")),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    let out = ctx.render()?;

    assert!(out.contains("# Post-Epic Review"));
    assert!(out.contains("Base commit**: abc1234"));
    assert!(out.contains("Molecule**: wx-3hhwq"));
    assert!(out.contains("git diff abc1234..HEAD"));
    assert!(out.contains("- wx-3hhwq.10: closed"));
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
