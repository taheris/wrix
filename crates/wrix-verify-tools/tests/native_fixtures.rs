#![cfg(test)]
mod common;

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::{Command, Output},
};

const MEMBERS: [(&str, &str); 12] = [
    ("WRIX_TEST_PREK_HOOKS", "hooks"),
    ("WRIX_TEST_PREK_RUNNER", "runner"),
    ("WRIX_TEST_BROKEN_PREK_RUNNER", "broken-runner"),
    ("WRIX_TEST_PREK_BUILD_SPEC", "builder-spec"),
    ("WRIX_TEST_PACKAGED_WRIX", "packaged-wrix"),
    ("WRIX_TEST_DEVSHELL_HOOK", "devshell-hook"),
    ("WRIX_TEST_CELLERD", "cellerd"),
    ("WRIX_TEST_CONTROLLER_BINDING", "controller-binding"),
    ("WRIX_TEST_SCCACHE", "compiler"),
    ("WRIX_TEST_SQL_CLIENT", "sql-client"),
    ("WRIX_TEST_BD", "beads"),
    ("WRIX_TEST_FIXTURE_RUNTIME", "fixture-runtime"),
];

struct Fixture {
    root: tempfile::TempDir,
    outputs: PathBuf,
    bundle: PathBuf,
    exported: PathBuf,
    calls: PathBuf,
    environment: BTreeMap<OsString, OsString>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("wrix fixtures ")
            .tempdir()
            .unwrap();
        let bundle = root.path().join("bundle");
        let outputs = root.path().join("outputs");
        fs::create_dir(&bundle).unwrap();
        fs::create_dir(&outputs).unwrap();
        for (name, member) in MEMBERS {
            let path = outputs.join(member);
            symlink(&path, bundle.join(member)).unwrap();
            if member == "hooks" {
                fs::create_dir(path).unwrap();
            } else {
                fs::write(&path, "fixture\n").unwrap();
                if name.contains("RUNNER")
                    || matches!(
                        name,
                        "WRIX_TEST_PACKAGED_WRIX"
                            | "WRIX_TEST_CELLERD"
                            | "WRIX_TEST_BD"
                            | "WRIX_TEST_SQL_CLIENT"
                            | "WRIX_TEST_FIXTURE_RUNTIME"
                    )
                {
                    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
                }
            }
        }
        let exported = root.path().join("environment");
        fs::write(&exported, "").unwrap();
        let calls = root.path().join("calls");
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let nix = bin.join("nix");
        fs::write(
            &nix,
            r#"#!/usr/bin/env bash
set -euo pipefail
printf 'call\n' >>"$WRIX_FIXTURE_CALLS"
printf '%s\n' "$*" >>"$WRIX_FIXTURE_CALLS.args"
if [[ "$WRIX_FIXTURE_STATUS" != 0 ]]; then
  echo fixture-build-failed >&2
  exit "$WRIX_FIXTURE_STATUS"
fi
printf '%s\n' "$WRIX_FIXTURE_BUNDLE"
"#,
        )
        .unwrap();
        fs::set_permissions(nix, fs::Permissions::from_mode(0o755)).unwrap();
        let mut environment: BTreeMap<_, _> = env::vars_os()
            .filter(|(name, _)| !MEMBERS.iter().any(|(key, _)| name == key))
            .collect();
        environment.remove(&OsString::from("BASH_ENV"));
        environment.remove(&OsString::from("ENV"));
        let path = env::join_paths(
            std::iter::once(bin).chain(env::split_paths(&env::var_os("PATH").unwrap())),
        )
        .unwrap();
        for (name, value) in [
            ("PATH", path),
            ("NEXTEST_ENV", exported.clone().into_os_string()),
            ("WRIX_FIXTURE_CALLS", calls.clone().into_os_string()),
            ("WRIX_FIXTURE_STATUS", OsString::from("0")),
            ("WRIX_FIXTURE_BUNDLE", bundle.clone().into_os_string()),
        ] {
            environment.insert(name.into(), value);
        }
        Self {
            root,
            outputs,
            bundle,
            exported,
            calls,
            environment,
        }
    }
    fn run(&self) -> Output {
        Command::new("bash")
            .arg(common::repo_file("tests/prepare-native-fixtures.sh"))
            .env_clear()
            .envs(&self.environment)
            .output()
            .unwrap()
    }
    fn expected(&self) -> BTreeMap<String, String> {
        MEMBERS
            .iter()
            .map(|(name, member)| {
                (
                    (*name).to_owned(),
                    self.outputs.join(member).to_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
    fn exported(&self) -> BTreeMap<String, String> {
        fs::read_to_string(&self.exported)
            .unwrap()
            .lines()
            .map(|line| {
                let (name, value) = line.split_once('=').unwrap();
                (name.to_owned(), value.to_owned())
            })
            .collect()
    }
}

#[test]
fn one_build_supplies_all_packaging_fixtures() {
    let fixture = Fixture::new();
    let output = fixture.run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.exported(), fixture.expected());
    assert_eq!(
        fs::read_to_string(&fixture.calls).unwrap().lines().count(),
        1
    );
    let args = fs::read_to_string(fixture.calls.with_extension("args")).unwrap();
    assert!(args.contains(&format!(
        "--out-link {}.fixtures",
        fixture.exported.display()
    )));
}
#[test]
fn existing_nix_packaging_inputs_need_no_nix_frontend() {
    let mut fixture = Fixture::new();
    let expected = fixture.expected();
    for (name, value) in &expected {
        fixture.environment.insert(name.into(), value.into());
    }
    let output = fixture.run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.exported(), expected);
    assert!(!fixture.calls.exists());
}
#[test]
fn partial_inputs_keep_the_explicit_override() {
    let mut fixture = Fixture::new();
    let custom = fixture.root.path().join("custom-runner");
    fs::write(&custom, "custom\n").unwrap();
    fs::set_permissions(&custom, fs::Permissions::from_mode(0o755)).unwrap();
    fixture.environment.insert(
        "WRIX_TEST_PREK_RUNNER".into(),
        custom.clone().into_os_string(),
    );
    let output = fixture.run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fixture.exported()["WRIX_TEST_PREK_RUNNER"],
        custom.to_str().unwrap()
    );
    assert_eq!(
        fs::read_to_string(fixture.calls).unwrap().lines().count(),
        1
    );
}
#[test]
fn build_failure_stops_before_exporting_any_fixture() {
    let mut fixture = Fixture::new();
    fixture
        .environment
        .insert("WRIX_FIXTURE_STATUS".into(), "23".into());
    let output = fixture.run();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("fixture-build-failed"));
    assert_eq!(fs::read_to_string(fixture.exported).unwrap(), "");
}
fn reject_non_executable(member: &str, name: &str) {
    let fixture = Fixture::new();
    fs::set_permissions(
        fixture.bundle.join(member),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let output = fixture.run();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains(name));
    assert_eq!(fs::read_to_string(fixture.exported).unwrap(), "");
}
#[test]
fn non_executable_runner_is_rejected_before_export() {
    reject_non_executable("runner", "WRIX_TEST_PREK_RUNNER");
}
#[test]
fn non_executable_celler_is_rejected_before_export() {
    reject_non_executable("cellerd", "WRIX_TEST_CELLERD");
}
#[test]
fn non_executable_beads_is_rejected_before_export() {
    reject_non_executable("beads", "WRIX_TEST_BD");
}
#[test]
fn non_executable_sql_client_is_rejected_before_export() {
    reject_non_executable("sql-client", "WRIX_TEST_SQL_CLIENT");
}
#[test]
fn non_executable_fixture_runtime_is_rejected_before_export() {
    reject_non_executable("fixture-runtime", "WRIX_TEST_FIXTURE_RUNTIME");
}
#[test]
fn newline_in_an_override_cannot_inject_an_environment_variable() {
    let mut fixture = Fixture::new();
    let path = fixture.root.path().join("runner\nINJECTED=value");
    fs::write(&path, "fixture\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    fixture
        .environment
        .insert("WRIX_TEST_PREK_RUNNER".into(), path.into_os_string());
    assert!(!fixture.run().status.success());
    assert_eq!(fs::read_to_string(fixture.exported).unwrap(), "");
}
#[test]
fn missing_nextest_environment_is_an_error() {
    let mut fixture = Fixture::new();
    fixture.environment.remove(&OsString::from("NEXTEST_ENV"));
    assert!(!fixture.run().status.success());
    assert!(!fixture.calls.exists());
}
