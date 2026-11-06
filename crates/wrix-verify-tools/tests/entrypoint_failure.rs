#![cfg(test)]

mod common;

use std::{fs, os::unix::fs::PermissionsExt, process::Command};

const CASES: [&str; 3] = [
    "test_linux_core_hooks_path",
    "test_darwin_core_hooks_path",
    "test_linked_worktree_core_hooks_path_both",
];

fn run_cases(cases: &[&str]) {
    let root = tempfile::Builder::new()
        .prefix("wrix-hook-failure-")
        .tempdir()
        .unwrap();
    let bash = Command::new("bash")
        .args(["-c", "printf '%s' \"$BASH\""])
        .env_remove("BASH_ENV")
        .env_remove("ENV")
        .output()
        .unwrap();
    assert!(
        bash.status.success(),
        "Bash is required for the entrypoint fixture"
    );
    let bash = String::from_utf8(bash.stdout).unwrap();
    assert!(std::path::Path::new(&bash).is_absolute());
    let runner = root.path().join("wrix-prek");
    fs::write(
        &runner,
        format!(
            "#!{bash}\nset -euo pipefail\nprintf 'fixture hook binding failed\\n' >&2\nexit 23\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&runner, fs::Permissions::from_mode(0o755)).unwrap();
    let driver = root.path().join("driver.sh");
    fs::write(
        &driver,
        format!(
            "review_hook_cases() {{\n  unset BASH_ENV\n  ALL_TESTS=({})\n  run_all\n}}\n",
            cases.join(" ")
        ),
    )
    .unwrap();
    let repository = fs::canonicalize(common::repo_file(".")).unwrap();
    let local_variables = Command::new("git")
        .args(["rev-parse", "--local-env-vars"])
        .current_dir(&repository)
        .output()
        .unwrap();
    assert!(
        local_variables.status.success(),
        "{}",
        String::from_utf8_lossy(&local_variables.stderr)
    );
    let mut command = Command::new(&bash);
    command
        .arg(repository.join("tests/sandbox/entrypoint-contract.sh"))
        .arg("review_hook_cases")
        .current_dir(&repository)
        .env("BASH_ENV", driver)
        .env("REPO_ROOT", &repository)
        .env(
            "PATH",
            std::env::join_paths(
                std::iter::once(root.path().to_path_buf())
                    .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
            )
            .unwrap(),
        );
    for name in String::from_utf8(local_variables.stdout).unwrap().lines() {
        command.env_remove(name);
    }
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!output.status.success(), "{stdout}{stderr}");
    assert_ne!(output.status.code(), Some(77), "{stderr}");
    assert!(stderr.contains("fixture hook binding failed"), "{stderr}");
    assert!(
        !format!("{stdout}{stderr}").contains("PASS:"),
        "{stdout}{stderr}"
    );
    for case in cases {
        assert!(stderr.contains(&format!("FAIL: {case}")), "{stderr}");
    }
}

#[test]
fn linux_failure_propagates() {
    run_cases(&CASES[..1]);
}

#[test]
fn darwin_failure_propagates() {
    run_cases(&CASES[1..2]);
}

#[test]
fn linked_worktree_failure_propagates() {
    run_cases(&CASES[2..]);
}

#[test]
fn aggregate_reports_all_owner_failures() {
    run_cases(&CASES);
}
