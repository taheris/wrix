//! Integration tests for the `loom-walk` binary's dispatcher and the
//! verifier-runner contract (env in, JSON out, exit code mirrors verdict).
//!
//! Each registered walk lands a pass-fixture and a fail-fixture in this
//! file: build synthetic source under `tempfile::tempdir`, point
//! `LOOM_FILES` at it, invoke the binary, assert the verdict and exit
//! code. The scaffolding bead registers zero walks, so the per-walk
//! fixtures arrive together with their walks in the walks bead. The
//! contract tests below remain — they cover the dispatcher itself.

#![allow(clippy::unwrap_used)]

use std::process::Command;

/// Invoke the built `loom-walk` binary with the supplied argv tail and
/// optional `LOOM_FILES` env value, returning the captured output. Tests
/// invoke the binary as a subprocess (rather than calling library code)
/// because the dispatcher's contract is process-shaped — stdout JSON line,
/// exit code, env var input — and a library-level test would bypass the
/// codepath the gate actually exercises.
fn invoke(args: &[&str], loom_files: Option<&str>) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_loom-walk");
    let mut cmd = Command::new(bin);
    cmd.args(args);
    match loom_files {
        Some(value) => {
            cmd.env("LOOM_FILES", value);
        }
        None => {
            cmd.env_remove("LOOM_FILES");
        }
    }
    cmd.output().unwrap()
}

#[test]
fn missing_walk_name_exits_two_and_names_available_walks() {
    let out = invoke(&[], None);
    let code = out.status.code().unwrap();
    assert_eq!(code, 2, "stderr={}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage: loom-walk"), "stderr={stderr}");
    assert!(
        stderr.contains("available walks"),
        "must enumerate available walks; stderr={stderr}"
    );
}

#[test]
fn unknown_walk_name_exits_two_and_names_the_walk_and_available_set() {
    let out = invoke(&["definitely_not_a_walk"], None);
    let code = out.status.code().unwrap();
    assert_eq!(code, 2, "stderr={}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown walk"),
        "must say 'unknown walk'; stderr={stderr}"
    );
    assert!(
        stderr.contains("definitely_not_a_walk"),
        "must echo the offending walk name; stderr={stderr}"
    );
    assert!(
        stderr.contains("available walks"),
        "must enumerate available walks; stderr={stderr}"
    );
}

#[test]
fn empty_registry_renders_available_walks_as_none_token() {
    let out = invoke(&["whatever"], None);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("<none>"),
        "empty registry must render '<none>' so the error reads cleanly; stderr={stderr}"
    );
}
