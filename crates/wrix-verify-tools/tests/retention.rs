#![cfg(test)]

use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const TOOL: &str = env!("CARGO_BIN_EXE_wrix-verification-roots");

fn setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let cache = temporary.path().join("cache");
    let manifest = temporary.path().join("manifest");
    fs::write(&manifest, "").unwrap();
    assert!(
        Command::new(TOOL)
            .arg("init")
            .arg(&cache)
            .status()
            .unwrap()
            .success()
    );
    (temporary, cache, manifest)
}

fn plan(cache: &Path, manifest: &Path) -> std::process::Output {
    Command::new(TOOL)
        .arg("plan")
        .arg(cache)
        .arg(manifest)
        .output()
        .unwrap()
}

#[test]
fn managed_execution_preserves_the_actual_commands_exit_status() {
    let (_temporary, cache, manifest) = setup();
    for code in [0, 37] {
        let status = Command::new(TOOL)
            .arg("run")
            .arg(&cache)
            .arg(&manifest)
            .args(["--", "sh", "-c", &format!("exit {code}")])
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(code));
        assert!(plan(&cache, &manifest).status.success());
    }
}

#[test]
fn killed_parent_cannot_release_a_still_running_descendants_lease() {
    let (_temporary, cache, manifest) = setup();
    let mut child = Command::new(TOOL)
        .arg("run")
        .arg(&cache)
        .arg(&manifest)
        .args(["--", "sh", "-c", "sleep 2 & printf 'ready\\n'; wait"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    assert!(!plan(&cache, &manifest).status.success());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!plan(&cache, &manifest).status.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if plan(&cache, &manifest).status.success() {
            break;
        }
        assert!(Instant::now() < deadline, "descendant lease did not end");
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn recipe_manifests_are_refused_before_registration_or_execution() {
    let (_temporary, cache, manifest) = setup();
    fs::write(
        &manifest,
        "/nix/store/00000000000000000000000000000000-unwanted-build.drv\n",
    )
    .unwrap();
    for mode in ["plan", "replace", "run"] {
        let mut command = Command::new(TOOL);
        command.args([mode]).arg(&cache).arg(&manifest);
        if mode == "run" {
            command.args(["--", "must-not-execute"]);
        }
        let output = command.env("PATH", "").output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("derivation recipes cannot be used as output roots")
        );
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 2);
    }
}

fn private_temporary() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

fn session(cache: &Path, script: &str) -> Command {
    let mut command = Command::new(TOOL);
    command
        .arg("session")
        .arg(cache)
        .args(["--", "bash", "-c", script]);
    command
}

fn clean_sessions(cache: &Path) -> std::process::Output {
    Command::new(TOOL)
        .arg("session-clean")
        .arg(cache)
        .output()
        .unwrap()
}

#[test]
fn session_execution_preserves_each_exit_class_and_bounds_retired_batches() {
    let temporary = private_temporary();
    let cache = temporary.path().join("sessions");
    for code in [0, 37, 77] {
        let output = session(
            &cache,
            &format!("printf output; printf error >&2; exit {code}"),
        )
        .output()
        .unwrap();
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        assert_eq!(output.stdout, b"output");
        assert_eq!(output.stderr, b"error");
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 3);
    }
    assert!(clean_sessions(&cache).status.success());
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 2);
    let status = session(&cache, "kill -TERM $$").status().unwrap();
    assert_eq!(status.signal(), Some(15));
    assert!(clean_sessions(&cache).status.success());
}

#[test]
fn killed_session_keeps_roots_until_the_last_inherited_descriptor_closes() {
    let temporary = private_temporary();
    let cache = temporary.path().join("sessions");
    let mut child = session(&cache, "ln -s /nix/store/00000000000000000000000000000000-fixture \"$WRIX_CI_BATCH_DIR/runner\"; sleep 2 & printf '%s\\n' \"$WRIX_CI_BATCH_DIR\"; wait")
        .stdout(Stdio::piped()).spawn().unwrap();
    let mut path = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut path)
        .unwrap();
    let batch = std::path::PathBuf::from(path.trim());
    assert!(batch.join("runner").is_symlink());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(clean_sessions(&cache).status.success());
    assert!(batch.join("runner").is_symlink());
    assert!(session(&cache, "exit 0").status().unwrap().success());
    assert!(batch.join("runner").is_symlink());
    let deadline = Instant::now() + Duration::from_secs(5);
    while batch.exists() {
        assert!(Instant::now() < deadline);
        assert!(clean_sessions(&cache).status.success());
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 2);
}

#[test]
fn concurrent_sessions_publish_complete_private_namespaces_without_serializing_commands() {
    let temporary = private_temporary();
    let cache = temporary.path().join("sessions");
    let barrier = temporary.path().join("barrier");
    fs::create_dir(&barrier).unwrap();
    thread::scope(|scope| {
        for _ in 0..8 {
            let cache = &cache;
            let barrier = &barrier;
            scope.spawn(move || {
                let output = session(
                    cache,
                    r#"set -euo pipefail
mkdir "$SESSION_BARRIER/$$"
for attempt in {1..200}; do
  members=("$SESSION_BARRIER"/*)
  if [[ "${#members[@]}" == 8 ]]; then exit 0; fi
  sleep 0.01
done
exit 19
"#,
                )
                .env("SESSION_BARRIER", barrier)
                .output()
                .unwrap();
                assert!(output.status.success(), "{output:?}");
            });
        }
    });
    assert!(clean_sessions(&cache).status.success());
    assert_eq!(fs::read_dir(cache).unwrap().count(), 2);
}

#[test]
fn session_refuses_foreign_namespace_before_the_requested_command() {
    let temporary = private_temporary();
    let cache = temporary.path().join("foreign");
    fs::create_dir(&cache).unwrap();
    fs::write(cache.join("foreign"), "untouched").unwrap();
    let output = session(&cache, "printf must-not-run").output().unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
    assert_eq!(fs::read(cache.join("foreign")).unwrap(), b"untouched");
}

#[test]
fn dry_run_does_not_create_generations() {
    let (_temporary, cache, manifest) = setup();
    for _ in 0..3 {
        assert!(plan(&cache, &manifest).status.success());
    }
    assert_eq!(fs::read_dir(cache).unwrap().count(), 2);
}
