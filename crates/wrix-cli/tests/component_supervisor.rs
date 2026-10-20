#![cfg(target_os = "linux")]

mod common;

use std::{
    fs,
    os::unix::fs::symlink,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use common::TestResult;
use wrix_core::path::Workspace;

#[test]
fn dolt_spawn_failure_does_not_leave_the_owned_celler_process_running() -> TestResult {
    let root = tempfile::tempdir()?;
    let bin = root.path().join("bin");
    fs::create_dir(&bin)?;
    symlink(common::command_path("sh")?, bin.join("sh"))?;
    symlink(common::command_path("kill")?, bin.join("kill"))?;
    let state = root.path().join("component");
    let pid = root.path().join("first.pid");
    let namespace = Workspace::from_path(root.path())?;
    let log = root.path().join("supervisor.log");
    let first = format!(
        "echo $$ > '{}'; exec '{}' 5",
        pid.display(),
        common::command_path("sleep")?.display(),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_wrix-service-supervisor"))
        .arg("serve")
        .arg(&state)
        .arg(namespace.hash().as_str())
        .arg(root.path().join("dolt.sock"))
        .args([first.as_str(), "exit 0"])
        .env("PATH", &bin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(&log)?)
        .output()?;
    assert!(!output.status.success());
    let diagnostic = fs::read_to_string(log)?;
    assert!(
        diagnostic.contains("No such file or directory"),
        "{diagnostic}"
    );
    let deadline = Instant::now() + Duration::from_millis(500);
    while !pid.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    if pid.exists() {
        let pid = fs::read_to_string(&pid)?;
        let alive = Command::new("kill")
            .args(["-0", pid.trim()])
            .output()?
            .status
            .success();
        if alive {
            let cleanup = Command::new("kill").args(["-TERM", pid.trim()]).output()?;
            assert!(cleanup.status.success());
        }
        assert!(!alive, "failed component startup orphaned Celler PID {pid}");
    }
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(state.join("component.json"))?)?;
    assert_eq!(record["epoch"], "clean");
    Ok(())
}
