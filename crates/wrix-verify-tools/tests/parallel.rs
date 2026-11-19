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
fn distinct_targets_keep_original_order_duplicates_statuses_and_four_job_bound() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    let targets = ["alpha", "beta", "gamma", "delta", "epsilon", "alpha"];
    fs::write(
        work.join("targets.json"),
        serde_json::to_vec(&targets).unwrap(),
    )
    .unwrap();
    for target in &targets[..5] {
        fs::write(
            root.path().join(target),
            r#"set -euo pipefail
mkdir "$CASE_ROOT/run-$BASHPID"
touch "$CASE_ROOT/started-$0"
for iteration in {1..500}; do
  [[ ! -e "$CASE_ROOT/release" ]] || break
  sleep 0.01
done
[[ -e "$CASE_ROOT/release" ]]
printf '%s\n' "$0"
printf 'stderr-%s\n' "$0" >&2
if [[ "$0" == beta ]]; then exit 37; fi
"#,
        )
        .unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_wrix-parallel-verifiers"))
        .arg(&work)
        .arg("bash")
        .arg("@targets")
        .arg(targets.len().to_string())
        .arg(std::process::id().to_string())
        .current_dir(root.path())
        .env("CASE_ROOT", root.path())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    for target in &targets[..4] {
        while !root.path().join(format!("started-{target}")).exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(!root.path().join("started-epsilon").exists());
    fs::write(root.path().join("release"), "").unwrap();
    assert!(child.wait().unwrap().success());
    for (index, target) in targets.iter().enumerate() {
        assert_eq!(
            fs::read_to_string(work.join(format!("{index}.stdout"))).unwrap(),
            format!("{target}\n")
        );
        assert_eq!(
            fs::read_to_string(work.join(format!("{index}.stderr"))).unwrap(),
            format!("stderr-{target}\n")
        );
        assert_eq!(
            fs::read_to_string(work.join(format!("{index}.status"))).unwrap(),
            if *target == "beta" { "37" } else { "0" }
        );
    }
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("run-"))
            .count(),
        6
    );
}

#[test]
fn shell_dispatch_preserves_interleaved_owner_receipts_and_excludes_unsafe_targets() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    for (name, content) in [
        (
            "verify",
            "#!/usr/bin/env bash\nset -euo pipefail\ntouch \"$CASE_ROOT/execution-$BASHPID\"\nprintf '%s\\n' \"$1\"\nprintf 'stderr-%s\\n' \"$1\" >&2\nif [[ \"$1\" == beta ]]; then exit 37; fi\n",
        ),
        (
            "wrix-verification-roots",
            "#!/usr/bin/env bash\nset -euo pipefail\nif [[ \"$1\" == init ]]; then mkdir -p \"$2\"; exit 0; fi\n[[ \"$1\" == run && \"$4\" == -- ]]\ncp \"$3\" \"$CASE_ROOT/retained\"\nshift 4\nexec \"$@\"\n",
        ),
    ] {
        let path = bin.join(name);
        fs::write(&path, content).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::os::unix::fs::symlink(
        env!("CARGO_BIN_EXE_wrix-parallel-verifiers"),
        bin.join("wrix-parallel-verifiers"),
    )
    .unwrap();
    let source = root.path().join("parallel-verifiers.sh");
    fs::write(
        &source,
        include_str!("../../../tests/lib/parallel-verifiers.sh"),
    )
    .unwrap();
    let output = Command::new("bash")
        .args([
            "-euo",
            "pipefail",
            "-c",
            r#"
source "$DISPATCH"
SELF="$CASE_ROOT/bin/verify"
export XDG_CACHE_HOME="$CASE_ROOT/cache"
trap parallel_verifiers_cleanup EXIT
normalize_target() { printf '%s\n' "${1#verify:}"; }
parallel_verifier_target() { [[ "$1" == alpha || "$1" == beta || "$1" == gamma ]]; }
nix() { printf '%s\n' "$CASE_ROOT"; }
verify_script_path() { printf '%s\n' "$CASE_ROOT/$3"; }
parallel_verifiers_prepare verify:alpha beta alpha unsafe gamma
[[ "$parallel_verifier_total" == 4 ]]
parallel_verifier_run source system alpha
if parallel_verifier_run source system beta; then exit 99; else [[ "$?" == 37 ]]; fi
parallel_verifier_run source system alpha
parallel_verifier_run source system gamma
"#,
        ])
        .env("DISPATCH", source)
        .env("CASE_ROOT", root.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"alpha\nbeta\nalpha\ngamma\n");
    assert_eq!(
        output.stderr,
        b"stderr-alpha\nstderr-beta\nstderr-alpha\nstderr-gamma\n"
    );
    let retained = fs::read_to_string(root.path().join("retained")).unwrap();
    for target in ["alpha", "beta", "gamma"] {
        assert!(
            retained
                .lines()
                .any(|line| line == root.path().join(target).to_str().unwrap())
        );
    }
    assert!(!retained.contains("unsafe"));
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("execution-"))
            .count(),
        4
    );
}

#[test]
fn target_manifest_refuses_malformed_oversized_count_mismatched_and_unsafe_jobs() {
    let oversized = vec![b' '; 65_537];
    for content in [
        b"{}".as_slice(),
        b"[]",
        b"[\"alpha\",\"beta;false\"]",
        b"[\"alpha\",\"\"]",
        oversized.as_slice(),
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("targets.json"), content).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_wrix-parallel-verifiers"))
            .arg(root.path())
            .arg("false")
            .arg("@targets")
            .arg("2")
            .arg(std::process::id().to_string())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!root.path().join("0.stdout").exists());
    }
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
fn signaled_workers_publish_the_actual_signal_exit_class() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    let script = root.path().join("signal.sh");
    fs::write(&script, "kill -TERM $$\n").unwrap();
    assert!(command(&work, &script).status().unwrap().success());
    for index in 0..4 {
        assert_eq!(
            fs::read_to_string(work.join(format!("{index}.status"))).unwrap(),
            "143"
        );
    }
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
