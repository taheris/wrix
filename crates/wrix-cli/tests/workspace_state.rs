//! Actual host workspace state and admission entrypoints.

#![cfg(test)]

#[path = "workspace/fixture.rs"]
mod fixture;

use fixture::{Fixture, snapshot};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

#[test]
fn worker_cannot_initialize_host_authority_through_cli_or_library() {
    if !Path::new("/etc/wrix/image-agent").exists() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wrix"))
        .args(["workspace", "initialize"])
        .env("HOME", home.path())
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("host-only"));
    assert!(wrix_state::host::Host::discover().is_err());
    assert!(fs::read_dir(home.path()).unwrap().next().is_none());
}

#[test]
fn initializes_private_database_without_acknowledging_scope() {
    let fixture = Fixture::new();
    fixture.initialize();
    let before = snapshot(&fixture.state());
    fixture.initialize();
    assert_eq!(snapshot(&fixture.state()), before);
    let path = fixture.state().join("state.db");
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let output = fixture.run(&["inspect", "--checkout", "/"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("standalone_allocation_acknowledged: false")
    );
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn acknowledgment_is_independent_idempotent_and_inspection_is_read_only() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .run(&["acknowledge-scope", "--version", "1"])
            .status
            .success()
    );
    let before = snapshot(&fixture.state());
    assert!(
        fixture
            .run(&["acknowledge-scope", "--version", "1"])
            .status
            .success()
    );
    assert_eq!(snapshot(&fixture.state()), before);
    let output = fixture.run(&["integration", "disable"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not initialized"));
    let output = fixture.run(&["inspect", "--checkout", "/"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("standalone_allocation_acknowledged: true")
    );
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn absent_store_inspection_creates_nothing() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .run(&["inspect", "--checkout", "/"])
            .status
            .success()
    );
    assert!(!fixture.state().exists());
}

#[test]
fn malformed_or_missing_expected_database_is_not_reset() {
    let fixture = Fixture::new();
    fixture.initialize();
    fs::write(fixture.state().join("state.db"), "not a SQLite database").unwrap();
    let before = snapshot(&fixture.state());
    for args in [
        vec!["initialize"],
        vec!["acknowledge-scope", "--version", "1"],
        vec!["inspect", "--checkout", "/"],
    ] {
        assert!(!fixture.run(&args).status.success());
        assert_eq!(snapshot(&fixture.state()), before);
    }
    fs::remove_file(fixture.state().join("state.db")).unwrap();
    assert!(!fixture.run(&["initialize"]).status.success());
    assert!(!fixture.state().join("state.db").exists());
}

#[test]
fn concurrent_initializers_publish_one_database() {
    let fixture = Fixture::new();
    let mut children = (0..6)
        .map(|_| fixture.command(&["initialize"]).spawn().unwrap())
        .collect::<Vec<_>>();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    let before = snapshot(&fixture.state());
    fixture.initialize();
    assert_eq!(snapshot(&fixture.state()), before);
}
