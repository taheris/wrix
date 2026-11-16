#![cfg(test)]

use std::{
    fs,
    io::{BufRead, BufReader},
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

#[test]
fn dry_run_does_not_create_generations() {
    let (_temporary, cache, manifest) = setup();
    for _ in 0..3 {
        assert!(plan(&cache, &manifest).status.success());
    }
    assert_eq!(fs::read_dir(cache).unwrap().count(), 2);
}
