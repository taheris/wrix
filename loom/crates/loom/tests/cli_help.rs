//! `insta` snapshot tests for `loom --help` and every subcommand `--help`.
//!
//! `--help` output IS the user contract: clap's auto-generated layout, flag
//! ordering, and default-value rendering are surfaces a human reads. Substring
//! assertions miss subtle drift (a renamed flag whose name still contains the
//! checked substring slips past). Snapshots fail loudly on any text change.
//!
//! Snapshot updates require a "snapshot updated because: ..." line in the PR
//! description so accidental drift is caught at review time.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

fn loom_help(args: &[&str]) -> String {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .args(args)
        .arg("--help")
        // Pin the rendered width so terminal-size variation cannot shift
        // wrapping between the developer's TTY and CI.
        .env("COLUMNS", "100")
        .env("CLAP_TERM_WIDTH", "100")
        .output()
        .expect("spawn loom");
    assert!(
        output.status.success(),
        "loom {args:?} --help exited non-zero: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("utf-8")
}

#[test]
fn loom_help_snapshot() {
    insta::assert_snapshot!(loom_help(&[]));
}

/// Independent of the snapshot: assert the spec-required section order and
/// per-section subcommand membership in `loom --help`. The snapshot test
/// catches any wording or layout drift; this test fails loudly if the
/// section ordering itself regresses, with a more specific diagnostic.
/// Spec: `loom-harness.md` § Functional #1.
#[test]
fn loom_help_groups_workflow_inspection_state_in_order() {
    let out = loom_help(&[]);
    let workflow = out.find("Workflow:").expect("Workflow heading missing");
    let inspection = out.find("Inspection:").expect("Inspection heading missing");
    let state = out.find("State:").expect("State heading missing");
    assert!(
        workflow < inspection && inspection < state,
        "headings must appear in order Workflow → Inspection → State, got {workflow} / {inspection} / {state}",
    );

    let workflow_section = &out[workflow..inspection];
    for sub in ["plan", "todo", "run", "check", "msg"] {
        assert!(
            workflow_section.contains(&format!("  {sub} "))
                || workflow_section.contains(&format!("  {sub}\n")),
            "Workflow section is missing `{sub}` row:\n{workflow_section}",
        );
    }

    let inspection_section = &out[inspection..state];
    for sub in ["status", "logs", "spec"] {
        assert!(
            inspection_section.contains(&format!("  {sub} "))
                || inspection_section.contains(&format!("  {sub}\n")),
            "Inspection section is missing `{sub}` row:\n{inspection_section}",
        );
    }

    let state_section = &out[state..];
    for sub in ["init", "use", "note"] {
        assert!(
            state_section.contains(&format!("  {sub} "))
                || state_section.contains(&format!("  {sub}\n")),
            "State section is missing `{sub}` row:\n{state_section}",
        );
    }

    // `doctor` was folded into `loom check --check=<name>` and must not
    // surface in the top-level help — its absence is part of the user
    // contract (spec § Functional #1).
    assert!(
        !out.contains("\n  doctor "),
        "`doctor` must not appear in `loom --help` output:\n{out}",
    );
}

#[test]
fn loom_init_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["init"]));
}

#[test]
fn loom_status_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["status"]));
}

#[test]
fn loom_use_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["use"]));
}

#[test]
fn loom_logs_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["logs"]));
}

#[test]
fn loom_spec_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["spec"]));
}

#[test]
fn loom_plan_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["plan"]));
}

#[test]
fn loom_run_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["run"]));
}

#[test]
fn loom_check_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["check"]));
}

#[test]
fn loom_check_rejects_unknown_check_selector() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .args(["check", "--check=bogus"])
        .env("COLUMNS", "100")
        .env("CLAP_TERM_WIDTH", "100")
        .output()
        .expect("spawn loom");
    assert!(
        !output.status.success(),
        "loom check --check=bogus must exit non-zero",
    );
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(
        stderr.contains("invalid value 'bogus'"),
        "stderr must name the invalid value, got: {stderr}",
    );
    for selector in ["criteria", "removals", "infrastructure", "cross-spec"] {
        assert!(
            stderr.contains(selector),
            "stderr must list `{selector}` as an allowed selector, got: {stderr}",
        );
    }
}

#[test]
fn loom_check_scope_flags_are_mutually_exclusive() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    for args in [
        vec!["check", "--bead", "wx-1", "--diff", "HEAD~1..HEAD"],
        vec!["check", "--bead", "wx-1", "--tree"],
        vec!["check", "--diff", "HEAD~1..HEAD", "--tree"],
    ] {
        let output = Command::new(loom_bin)
            .args(&args)
            .env("COLUMNS", "100")
            .env("CLAP_TERM_WIDTH", "100")
            .output()
            .expect("spawn loom");
        assert!(
            !output.status.success(),
            "loom {args:?} must exit non-zero (scope flags are mutually exclusive)",
        );
        let stderr = String::from_utf8(output.stderr).expect("utf-8");
        assert!(
            stderr.contains("cannot be used with"),
            "stderr must explain mutual exclusion, got: {stderr}",
        );
    }
}

#[test]
fn loom_msg_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["msg"]));
}

#[test]
fn loom_todo_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["todo"]));
}

#[test]
fn loom_note_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note"]));
}

#[test]
fn loom_note_set_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note", "set"]));
}

#[test]
fn loom_note_add_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note", "add"]));
}

#[test]
fn loom_note_clear_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note", "clear"]));
}

#[test]
fn loom_note_list_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note", "list"]));
}

#[test]
fn loom_note_rm_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note", "rm"]));
}
