//! `insta` snapshot tests for `loom --help` and every subcommand `--help`.
//!
//! `--help` output IS the user contract: clap's auto-generated layout, flag
//! ordering, and default-value rendering are surfaces a human reads. Substring
//! assertions miss subtle drift (a renamed flag whose name still contains the
//! checked substring slips past). Snapshots fail loudly on any text change.
//!
//! Snapshot updates require a "snapshot updated because: ..." line in the PR
//! description so accidental drift is caught at review time.

#![allow(clippy::unwrap_used, clippy::expect_used)]

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
fn loom_msg_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["msg"]));
}

#[test]
fn loom_todo_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["todo"]));
}
