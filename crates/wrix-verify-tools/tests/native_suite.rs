#![cfg(test)]
mod common;
use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
};
struct Fixture {
    root: tempfile::TempDir,
    trace: PathBuf,
    owner: PathBuf,
    command: Command,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let tmp = root.path().join("tmp");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&tmp).unwrap();
        let trace = root.path().join("suite-path");
        let owner = root.path().join("owner-pid");
        let bundle = root.path().join("bundle");
        fs::create_dir(&bundle).unwrap();
        let cargo = bin.join("cargo");
        fs::write(&cargo,r#"#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == 'nextest run --workspace --all-targets' ]]
printf '%s\n' "$WRIX_NATIVE_FIXTURE_ROOT_DIR" > "$WRIX_SUITE_TRACE"
ln -s "$WRIX_SUITE_BUNDLE" "$WRIX_NATIVE_FIXTURE_ROOT_DIR/fixtures"
[[ -L "$WRIX_NATIVE_FIXTURE_ROOT_DIR/fixtures" ]]
case "$WRIX_SUITE_MODE" in
  success) exit 0;;
  failure) exit 23;;
  interrupt)
    for attempt in {1..100}; do
      if [[ -s "$WRIX_SUITE_OWNER_FILE" ]]; then break; fi
      sleep 0.01
    done
    IFS= read -r wrapper < "$WRIX_SUITE_OWNER_FILE"
    [[ "$wrapper" =~ ^[0-9]+$ && "$wrapper" -gt 1 ]]
    trap '[[ -L "$WRIX_NATIVE_FIXTURE_ROOT_DIR/fixtures" ]]; printf reaped > "$WRIX_SUITE_TRACE.cancelled"; exit 143' TERM
    kill -TERM "$wrapper"
    while :; do sleep 0.05; done;;
  foreign-file) rm "$WRIX_NATIVE_FIXTURE_ROOT_DIR/fixtures"; printf foreign > "$WRIX_NATIVE_FIXTURE_ROOT_DIR/fixtures"; exit 0;;
  replaced-directory) mv "$WRIX_NATIVE_FIXTURE_ROOT_DIR" "$WRIX_NATIVE_FIXTURE_ROOT_DIR.saved"; mkdir "$WRIX_NATIVE_FIXTURE_ROOT_DIR"; printf foreign > "$WRIX_NATIVE_FIXTURE_ROOT_DIR/foreign"; exit 0;;
  *) exit 92;;
esac
"#).unwrap();
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o700)).unwrap();
        let path = env::join_paths(
            std::iter::once(bin).chain(env::split_paths(&env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let mut command = Command::new("bash");
        command
            .arg(common::repo_file("bin/test-native"))
            .env("PATH", path)
            .env("TMPDIR", tmp)
            .env("WRIX_SUITE_TRACE", &trace)
            .env("WRIX_SUITE_OWNER_FILE", &owner)
            .env("WRIX_SUITE_BUNDLE", bundle)
            .env("WRIX_SUITE_MODE", mode)
            .env_remove("BASH_ENV")
            .env_remove("ENV");
        Self {
            root,
            trace,
            owner,
            command,
        }
    }
    fn run(&mut self) -> Output {
        let child = self
            .command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        fs::write(&self.owner, format!("{}\n", child.id())).unwrap();
        child.wait_with_output().unwrap()
    }
    fn suite(&self) -> PathBuf {
        fs::read_to_string(&self.trace).unwrap().trim().into()
    }
}
#[test]
fn successful_complete_suite_releases_only_its_own_fixture_root() {
    let mut fixture = Fixture::new("success");
    let neighbor = fixture.root.path().join("neighbor");
    fs::write(&neighbor, b"keep").unwrap();
    let output = fixture.run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.suite().exists());
    assert_eq!(fs::read(neighbor).unwrap(), b"keep");
    assert!(String::from_utf8_lossy(&output.stdout).contains("limit: 300s"));
}
#[test]
fn test_failure_retains_real_exit_and_still_releases_fixture_root() {
    let mut fixture = Fixture::new("failure");
    let output = fixture.run();
    assert_eq!(output.status.code(), Some(23));
    assert!(!fixture.suite().exists());
}
#[test]
fn interruption_reaps_the_runner_before_releasing_its_fixture_root() {
    let mut fixture = Fixture::new("interrupt");
    let output = fixture.run();
    assert_eq!(
        output.status.code(),
        Some(143),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(fixture.trace.with_extension("cancelled")).unwrap(),
        b"reaped"
    );
    assert!(!fixture.suite().exists());
}
#[test]
fn changed_fixture_root_type_is_preserved_and_prevents_success() {
    let mut fixture = Fixture::new("foreign-file");
    let output = fixture.run();
    assert!(!output.status.success());
    assert_eq!(
        fs::read(fixture.suite().join("fixtures")).unwrap(),
        b"foreign"
    );
}
#[test]
fn replaced_suite_directory_is_preserved_and_prevents_success() {
    let mut fixture = Fixture::new("replaced-directory");
    let output = fixture.run();
    assert!(!output.status.success());
    assert_eq!(
        fs::read(fixture.suite().join("foreign")).unwrap(),
        b"foreign"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("identity changed"));
}
