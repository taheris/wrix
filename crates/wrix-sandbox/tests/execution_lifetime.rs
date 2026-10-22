use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use wrix_sandbox::execution::{self, Observation};
use wrix_state::{
    context::Context,
    execution::{self as state, Phase, Ticket},
    host::Host,
    store::Store,
};

fn child(root: &Path, action: &str) -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "execution_fixture_child", "--nocapture"])
        .env("WRIX_EXECUTION_FIXTURE", root)
        .env("WRIX_EXECUTION_ACTION", action)
        .env("XDG_STATE_HOME", root.join("state"));
    Ok(command)
}

fn run(root: &Path, action: &str) -> std::io::Result<()> {
    let output = child(root, action)?.output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn execution_fixture_child() {
    let Some(root) = std::env::var_os("WRIX_EXECUTION_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    let action = std::env::var("WRIX_EXECUTION_ACTION").unwrap();
    let host = Host::discover().unwrap();
    if action == "reserve" {
        Store::initialize(&host).unwrap();
        Store::acknowledge(&host, 1).unwrap();
        fs::create_dir_all(root.join("signals")).unwrap();
        let ticket = execution::reserve(&root).unwrap();
        fs::write(root.join("ticket"), ticket.as_str()).unwrap();
        return;
    }
    let ticket = Ticket::parse(&fs::read_to_string(root.join("ticket")).unwrap()).unwrap();
    let context = Context::inspect(&host, &root).unwrap();
    match action.as_str() {
        "projection-unknown" => {
            let before = state::observe(&host, &context, &ticket).unwrap();
            assert!(execution::projection(&root, &ticket).is_err());
            assert_eq!(state::observe(&host, &context, &ticket).unwrap(), before);
        }
        "projection" => {
            let projection = execution::projection(&root, &ticket).unwrap();
            assert!(
                projection
                    .mounts
                    .iter()
                    .any(|mount| mount.source == root.join("signals")
                        && mount.destination == Path::new("/work")
                        && mount.writable)
            );
            let value = serde_json::to_value(&projection).unwrap();
            assert!(value.get("Config").is_none());
            assert_eq!(
                state::observe(&host, &context, &ticket).unwrap(),
                Some(Phase::Bound(projection.id))
            );
        }
        "begin" => {
            state::begin(&host, &context, &ticket).unwrap();
            assert!(state::begin(&host, &context, &ticket).is_err());
        }
        "bind" => {
            state::begin(&host, &context, &ticket).unwrap();
            state::bind(
                &host,
                &context,
                &ticket,
                &state::Container::parse(&"1".repeat(64)).unwrap(),
            )
            .unwrap();
        }
        "unknown" => {
            assert!(execution::reconcile(&root, &ticket).is_err());
            assert!(matches!(
                state::observe(&host, &context, &ticket).unwrap(),
                Some(Phase::Bound(_))
            ));
        }
        "ambiguous" => {
            assert_eq!(
                execution::reconcile(&root, &ticket).unwrap(),
                Observation::Protected
            );
            assert!(matches!(
                state::observe(&host, &context, &ticket).unwrap(),
                Some(Phase::Bound(_))
            ));
        }
        "launching" => {
            assert_eq!(
                state::observe(&host, &context, &ticket).unwrap(),
                Some(Phase::Launching)
            );
            assert_eq!(
                execution::reconcile(&root, &ticket).unwrap(),
                Observation::Protected
            );
        }
        "launch" => {
            let mut command = Command::new("podman");
            command
                .args([
                    "run",
                    "--rm",
                    "--network=none",
                    "--memory=64m",
                    "--pids-limit=32",
                    "-v",
                ])
                .arg(format!("{}:/work", root.join("signals").display()))
                .args([
                    "docker.io/library/alpine:3.22",
                    "sh",
                    "-c",
                    "sleep 300 & echo ready > /work/ready; wait",
                ]);
            execution::launch(&root, &ticket, &command, false).unwrap();
        }
        "protected" => {
            assert_eq!(
                execution::reconcile(&root, &ticket).unwrap(),
                Observation::Protected
            );
            let Some(Phase::Bound(id)) = state::observe(&host, &context, &ticket).unwrap() else {
                panic!("missing trusted runtime binding");
            };
            fs::write(root.join("bound-id"), id.as_str()).unwrap();
        }
        "retired" => {
            assert_eq!(
                execution::reconcile(&root, &ticket).unwrap(),
                Observation::Retired
            );
            assert_eq!(
                execution::reconcile(&root, &ticket).unwrap(),
                Observation::Retired
            );
            assert!(state::begin(&host, &context, &ticket).is_err());
        }
        other => panic!("unsupported fixture action {other}"),
    }
}

#[test]
fn execution_restart_retains_unbound_launch_and_ignores_workspace_completion() {
    let root = tempfile::tempdir().unwrap();
    run(root.path(), "reserve").unwrap();
    run(root.path(), "projection-unknown").unwrap();
    run(root.path(), "begin").unwrap();
    fs::create_dir(root.path().join(".wrix")).unwrap();
    fs::write(
        root.path().join(".wrix/completed"),
        "inactive; launcher exited; PID missing",
    )
    .unwrap();
    run(root.path(), "launching").unwrap();
}

#[test]
fn execution_unavailable_ambiguous_and_failed_removal_observations_retain_binding() {
    use std::os::unix::fs::PermissionsExt;
    for (response, action) in [
        ("exit 125", "unknown"),
        ("printf invalid", "unknown"),
        ("printf invalid", "projection-unknown"),
        (
            r#"printf '{"Id":"2222222222222222222222222222222222222222222222222222222222222222","Mounts":[]}'"#,
            "projection-unknown",
        ),
        ("printf '[]'", "ambiguous"),
        (
            "printf '[{\"Id\":\"2222222222222222222222222222222222222222222222222222222222222222\",\"State\":{\"Status\":\"exited\",\"Running\":false,\"Paused\":false,\"Restarting\":false}}]'",
            "ambiguous",
        ),
        (
            "if [[ \"$1\" == rm ]]; then exit 125; fi; printf '[{\"Id\":\"1111111111111111111111111111111111111111111111111111111111111111\",\"State\":{\"Status\":\"exited\",\"Running\":false,\"Paused\":false,\"Restarting\":false}}]'",
            "unknown",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        run(root.path(), "reserve").unwrap();
        run(root.path(), "bind").unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let runtime = bin.join("podman");
        fs::write(
            &runtime,
            format!("#!/usr/bin/env bash\nset -euo pipefail\n{response}\n"),
        )
        .unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let output = child(root.path(), action)
            .unwrap()
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[ignore = "requires actual Linux rootless Podman and locally installed alpine:3.22"]
fn execution_actual_podman_survives_launcher_death_and_reconciles_exact_teardown() {
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap()
        .keep();
    eprintln!("trusted runtime lifetime evidence: {}", root.display());
    run(&root, "reserve").unwrap();
    let log = fs::File::create(root.join("launcher.log")).unwrap();
    let mut launcher = child(&root, "launch")
        .unwrap()
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !root.join("signals/ready").exists() {
        assert!(
            launcher.try_wait().unwrap().is_none(),
            "{}",
            fs::read_to_string(root.join("launcher.log")).unwrap()
        );
        assert!(Instant::now() < deadline, "fixture runtime start timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
    run(&root, "projection").unwrap();
    launcher.kill().unwrap();
    assert!(!launcher.wait().unwrap().success());
    run(&root, "protected").unwrap();
    let id = fs::read_to_string(root.join("bound-id")).unwrap();
    assert!(
        Command::new("podman")
            .args(["stop", "--time", "1", &id])
            .status()
            .unwrap()
            .success()
    );
    run(&root, "retired").unwrap();
    assert!(root.join("signals/ready").exists());
}
