use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn command(work: &Path, script: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wrix-parallel-verifiers"));
    command
        .arg(work)
        .arg("bash")
        .arg(script)
        .arg("4")
        .arg(std::process::id().to_string());
    command
}

#[test]
fn independent_jobs_overlap_and_preserve_output_and_all_exit_classes() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    let script = root.path().join("worker.sh");
    fs::write(&script, r#"set -euo pipefail
for slot in 0 1 2 3; do
  if mkdir "$CASE_ROOT/claim-$slot" 2>/dev/null; then break; fi
done
for iteration in {1..200}; do
  if [[ -d "$CASE_ROOT/claim-0" && -d "$CASE_ROOT/claim-1" && -d "$CASE_ROOT/claim-2" && -d "$CASE_ROOT/claim-3" ]]; then break; fi
  sleep 0.01
done
[[ -d "$CASE_ROOT/claim-3" ]]
printf '%s\n' "$slot"
printf 'stderr-%s\n' "$slot" >&2
case "$slot" in 0) exit 0;; 1) exit 1;; 2) exit 77;; 3) exit 37;; esac
"#).unwrap();
    let status = command(&work, &script)
        .env("CASE_ROOT", root.path())
        .status()
        .unwrap();
    assert!(status.success());
    let mut observed = Vec::new();
    for index in 0..4 {
        let stdout = fs::read_to_string(work.join(format!("{index}.stdout"))).unwrap();
        let slot: usize = stdout.trim().parse().unwrap();
        let stderr = fs::read_to_string(work.join(format!("{index}.stderr"))).unwrap();
        assert_eq!(stderr, format!("stderr-{slot}\n"));
        let code: i32 = fs::read_to_string(work.join(format!("{index}.status")))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(code, [0, 1, 77, 37][slot]);
        observed.push(slot);
    }
    observed.sort_unstable();
    assert_eq!(observed, [0, 1, 2, 3]);
}

#[test]
fn completed_job_cannot_leave_an_ordinary_descendant_running() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    let script = root.path().join("worker.sh");
    fs::write(
        &script,
        "set -euo pipefail\n(sleep 0.4; touch \"$CASE_ROOT/survived\") &\n",
    )
    .unwrap();
    assert!(
        command(&work, &script)
            .env("CASE_ROOT", root.path())
            .status()
            .unwrap()
            .success()
    );
    thread::sleep(Duration::from_millis(600));
    assert!(!root.path().join("survived").exists());
}

#[test]
fn cancellation_does_not_publish_success_for_incomplete_jobs() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    let script = root.path().join("worker.sh");
    fs::write(
        &script,
        "set -euo pipefail\ntouch \"$CASE_ROOT/ready\"\nsleep 30\n",
    )
    .unwrap();
    let mut child = command(&work, &script)
        .env("CASE_ROOT", root.path())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.path().join("ready").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    fs::write(work.join("cancel"), "").unwrap();
    assert!(!child.wait().unwrap().success());
    assert!(!work.join("0.status").exists());
}

#[cfg(test)]
fn calling_shell_death_stops_jobs(group_signal: bool) {
    use std::os::unix::process::CommandExt;
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    let script = root.path().join("worker.sh");
    fs::write(
        &script,
        "set -euo pipefail\ntouch \"$CASE_ROOT/ready\"\nsleep 1\ntouch \"$CASE_ROOT/survived\"\n",
    )
    .unwrap();
    let mut parent = Command::new("bash")
        .args(["-c", "set -euo pipefail; parent=$BASHPID; \"$SUPERVISOR\" \"$WORK\" bash \"$SCRIPT\" 4 \"$parent\" & wait"])
        .process_group(0)
        .env("SUPERVISOR", env!("CARGO_BIN_EXE_wrix-parallel-verifiers"))
        .env("WORK", &work).env("SCRIPT", &script).env("CASE_ROOT", root.path())
        .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.path().join("ready").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    if group_signal {
        let pid = rustix::process::Pid::from_raw(i32::try_from(parent.id()).unwrap()).unwrap();
        rustix::process::kill_process_group(pid, rustix::process::Signal::TERM).unwrap();
    } else {
        parent.kill().unwrap();
    }
    parent.wait().unwrap();
    thread::sleep(Duration::from_millis(1200));
    assert!(!root.path().join("survived").exists());
    assert!(!work.join("0.status").exists());
}

#[test]
fn killed_calling_shell_causes_supervisor_to_stop_its_owned_jobs() {
    calling_shell_death_stops_jobs(false);
}

#[test]
fn terminated_callers_process_group_cannot_orphan_the_separate_job_groups() {
    calling_shell_death_stops_jobs(true);
}
