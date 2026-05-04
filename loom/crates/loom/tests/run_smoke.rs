//! End-to-end smoke test for `loom run --once`.
//!
//! Spec acceptance criterion (wx-3hhwq.20): "loom run --once against a fake
//! bd returns a meaningful exit code (not unrecognized subcommand)".
//!
//! The test installs a stub `bd` on PATH that prints `[]` for every `ready` /
//! `list` query (emulating an empty molecule), seeds a state DB so spec
//! resolution succeeds, and invokes the compiled `loom` binary. The expected
//! path through `run_loop`:
//!
//! 1. `next_ready_bead` → `bd ready --label spec:<X>` → empty slice → `None`,
//! 2. `RunMode::Once` exits cleanly without invoking `loom check`,
//! 3. binary returns exit code 0.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// Write a stub `bd` shell script to `dir/bin/bd` that returns `[]` for any
/// JSON-shaped subcommand and `0` for everything else. Returns the bin
/// directory caller should prepend to PATH.
fn install_bd_stub(dir: &Path) -> std::path::PathBuf {
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bd = bin_dir.join("bd");
    std::fs::write(
        &bd,
        "#!/bin/sh\n\
         # The driver's `ready` / `list` calls all carry --json; the rest of\n\
         # the bd surface (close, update) gets a silent zero.\n\
         for arg in \"$@\"; do\n\
           if [ \"$arg\" = \"--json\" ]; then\n\
             printf '%s' '[]'\n\
             exit 0\n\
           fi\n\
         done\n\
         exit 0\n",
    )
    .unwrap();
    let mut perm = std::fs::metadata(&bd).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&bd, perm).unwrap();
    bin_dir
}

#[test]
fn loom_run_once_against_empty_bd_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join(".wrapix/loom")).unwrap();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();

    // Seed state DB + active spec so resolve_spec_label returns Some(label)
    // without the caller having to pass -s.
    let db = loom_core::state::StateDb::open(workspace.join(".wrapix/loom/state.db")).unwrap();
    db.set_current_spec(&loom_core::identifier::SpecLabel::new("loom-harness"))
        .unwrap();
    drop(db);

    let bin_dir = install_bd_stub(workspace);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![bin_dir];
    path_entries.extend(std::env::split_paths(&path));
    let new_path = std::env::join_paths(path_entries).unwrap();

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("run")
        .arg("--once")
        .env("PATH", new_path)
        // The exec_check path is gated behind RunMode::Continuous; on the
        // empty-queue path we still set this so the binary can locate itself
        // if the loop ever changes shape.
        .env("LOOM_BIN", loom_bin)
        .output()
        .expect("spawn loom");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom run --once must exit zero on empty queue. stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("loom run:"),
        "expected the run summary line. stdout={stdout}",
    );
    assert!(
        stdout.contains("molecule_complete=true"),
        "empty queue must mark the molecule complete. stdout={stdout}",
    );
    assert!(
        stdout.contains("execed_check=false"),
        "--once must NOT exec check. stdout={stdout}",
    );
}

#[test]
fn loom_run_recognizes_subcommand() {
    // Regression guard: before wx-3hhwq.20 the binary did not expose `run`,
    // so `loom run --help` printed "unrecognized subcommand: run". This test
    // pins the inverse — `--help` for the new subcommand exits cleanly.
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("run")
        .arg("--help")
        .output()
        .expect("spawn loom");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom run --help must exit zero. stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("--once") && stdout.contains("--parallel"),
        "loom run --help must list the spec'd flags. stdout={stdout}",
    );
}
