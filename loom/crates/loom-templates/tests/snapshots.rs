//! `insta` snapshot tests for every Askama template × representative input set.
//!
//! The rendered template body is the contract we ship to the agent — layout
//! drift slips silently past substring assertions. Snapshots surface the diff
//! in PR review. Updates require an explicit "snapshot updated because: ..."
//! line in the PR description (see `docs/style-guidelines.md`).
//!
//! One snapshot per typed context struct, named after the test function via
//! `insta::assert_snapshot!`'s default file naming.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use askama::Template;
use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};
use loom_templates::check::{CheckContext, ReviewSource};
use loom_templates::msg::{ClarifyBead, ClarifyOption, MsgContext};
use loom_templates::plan::{PlanNewContext, PlanUpdateContext};
use loom_templates::run::{PreviousFailure, RunContext};
use loom_templates::todo::{TodoNewContext, TodoUpdateContext};

const EXIT_SIGNALS_BODY: &str = "- `LOOM_COMPLETE`\n- `LOOM_BLOCKED`\n- `LOOM_CLARIFY`";
const PINNED_CONTEXT_BODY: &str =
    "# Project Overview\n\nLoom orchestrates the spec-to-implementation workflow.";
const SCRATCHPAD_PATH_BODY: &str = "/workspace/.wrapix/loom/scratch/loom-harness/scratch.md";

#[test]
fn plan_new_snapshot() {
    let ctx = PlanNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn plan_update_snapshot() {
    let ctx = PlanUpdateContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into(), "lib/ralph/template/".into()],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn todo_new_snapshot() {
    let ctx = TodoNewContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn todo_update_snapshot() {
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
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn run_snapshot() {
    let ctx = RunContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        molecule_id: Some(MoleculeId::new("wx-3hhwq")),
        issue_id: Some(BeadId::new("wx-3hhwq.10").unwrap()),
        title: Some("port templates".into()),
        description: Some("Port templates to Askama.".into()),
        previous_failure: Some(PreviousFailure::new("error: cargo test failed".to_string())),
        scratchpad_path: "/workspace/.wrapix/loom/scratch/wx-3hhwq.10/scratch.md".to_string(),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn check_snapshot() {
    let ctx = CheckContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        label: SpecLabel::new("loom-harness"),
        spec_path: "specs/loom-harness.md".to_string(),
        companion_paths: vec!["lib/sandbox/".into()],
        beads_summary: Some("- wx-3hhwq.10: closed".into()),
        base_commit: Some("abc1234".into()),
        molecule_id: Some(MoleculeId::new("wx-3hhwq")),
        verify_sources: vec![ReviewSource {
            path: "tests/loom-test.sh".into(),
            body: "test_review_inputs() { :; }\n".into(),
        }],
        judge_rubrics: vec![ReviewSource {
            path: "tests/judges/loom.sh".into(),
            body: "judge_live_path_coverage() { :; }\n".into(),
        }],
        scratchpad_path: SCRATCHPAD_PATH_BODY.to_string(),
        exit_signals: EXIT_SIGNALS_BODY.to_string(),
    };
    insta::assert_snapshot!(ctx.render().unwrap());
}

#[test]
fn msg_snapshot() {
    let ctx = MsgContext {
        pinned_context: PINNED_CONTEXT_BODY.to_string(),
        clarify_beads: vec![ClarifyBead {
            id: BeadId::new("wx-clar.1").unwrap(),
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
    insta::assert_snapshot!(ctx.render().unwrap());
}
