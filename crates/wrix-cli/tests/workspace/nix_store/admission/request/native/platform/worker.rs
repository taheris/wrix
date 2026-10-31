//! Actual native commands, durable worker bindings, independent broker lifetime and installed workflows.
use super::*;
use std::process::Stdio;
use wrix_sandbox::nix_store::Running as NativeRunning;

pub(super) struct Worker {
    pub(super) ticket: wrix_state::execution::Ticket,
    pub(super) root: PathBuf,
    pub(super) running: NativeRunning,
    pub(super) runtime: Command,
    native: serde_json::Value,
}
pub(super) fn recipe(command: &Command) -> Command {
    let mut copy = Command::new(command.get_program());
    copy.env_clear().envs(
        command
            .get_envs()
            .filter_map(|(key, value)| value.map(|value| (key, value))),
    );
    if let Some(directory) = command.get_current_dir() {
        copy.current_dir(directory);
    }
    copy.args(command.get_args());
    copy
}
pub(super) fn launch(
    platform: Platform,
    factory: &Connected,
    selection: &Selection,
    host: &wrix_state::host::Host,
    control: &Path,
    name: &str,
    binding: &wrix_core::nix_store::binding::Binding,
) -> Worker {
    let attempt = Attempt::reserve(selection, system(), binding.clone(), None).unwrap();
    let ticket = attempt.ticket().clone();
    let prepared = platform.prepare(attempt, factory);
    let root = prepared.delivery().mounts()[0]
        .source
        .parent()
        .unwrap()
        .to_owned();
    let mut command = prepared.command().unwrap();
    let runtime = recipe(&command);
    assert_eq!(
        Path::new(command.get_program()).file_name().unwrap(),
        if platform == Platform::Apple {
            "container"
        } else {
            "podman"
        }
    );
    command.args([
        "run",
        "--rm",
        "--cap-add",
        "CAP_NET_ADMIN",
        "--memory",
        "2048M",
    ]);
    match platform {
        Platform::Apple => {
            command.args([
                "--cpus",
                "2",
                "--network",
                "default",
                "--dns",
                "1.1.1.1",
                "--dns",
                "9.9.9.9",
            ]);
        }
        Platform::Krun | Platform::Podman => {
            command.args(["--pids-limit=256", "--network=pasta:--map-host-loopback,169.254.1.2,--map-guest-addr,none,-t,none,-u,none,-T,none,-U,none", "--mount", "type=tmpfs,destination=/home/wrix,U=true"]);
            if platform == Platform::Krun {
                command.arg("--runtime").arg(
                    prepared
                        .krun_runtime()
                        .expect("actual checked krun executable"),
                );
            } else {
                assert!(prepared.krun_runtime().is_none());
            }
        }
    }
    command.arg("-v").arg(format!(
        "{}:/workspace:rw",
        selection.context().unwrap().caller().display()
    ));
    command
        .arg("-v")
        .arg(format!("{}:/control:rw", control.display()));
    command.args([
        "-e",
        "HOME=/home/wrix",
        "-e",
        "WRIX_AGENT=direct",
        "-e",
        "WRIX_NETWORK=open",
        "-e",
        "GIT_TRACE=1",
        "-e",
        "GIT_TRACE_SETUP=1",
    ]);
    prepared.delivery().configure(&mut command);
    if platform == Platform::Apple {
        command.arg("--");
    }
    command
        .arg(if platform == Platform::Apple {
            if name == "replacement" {
                "wrix-pool-worker-seed-next:latest"
            } else {
                "wrix-pool-worker-seed:latest"
            }
        } else {
            binding.digest().as_str()
        })
        .arg(name);
    let mut running = prepared.spawn(&command, false).unwrap();
    // The factory owns stdio; guest reports are atomically published through the mounted control directory.
    let native = match platform {
        Platform::Apple => {
            let deadline = Instant::now() + BUDGET;
            loop {
                if root.join("broker.json").is_file() {
                    let record: serde_json::Value =
                        serde_json::from_slice(&fs::read(root.join("broker.json")).unwrap())
                            .unwrap();
                    assert!(record["worker"]["identity"]["name"].is_string());
                    let native = &record["worker"]["configuration"];
                    let proof = &record["worker"]["image"];
                    assert!(
                        native["creationDate"].is_string(),
                        "actual Apple ISO8601 birth required"
                    );
                    assert_eq!(native["image"]["descriptor"]["digest"], proof["index"]);
                    assert_eq!(proof["expected"], binding.digest().as_str());
                    assert_ne!(
                        proof["index"], proof["expected"],
                        "native index descriptor is not an OCI config digest"
                    );
                    break record;
                }
                assert!(running.child().try_wait().unwrap().is_none());
                assert!(
                    Instant::now() < deadline,
                    "Apple immutable worker binding was not persisted"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        Platform::Krun | Platform::Podman => serde_json::Value::Null,
    };
    assert!(
        wrix_state::execution::terminal(host, selection.context().unwrap().key(), &ticket)
            .unwrap()
            .is_none()
    );
    Worker {
        ticket,
        root,
        running,
        runtime,
        native,
    }
}
pub(super) fn milestone(control: &Path, file: &str, workers: &mut [&mut Worker]) {
    let deadline = Instant::now() + Duration::from_secs(600);
    while !control.join(file).exists() {
        for worker in workers.iter_mut() {
            assert!(
                worker.running.child().try_wait().unwrap().is_none(),
                "native worker exited before {file}; guest diagnostics: {}",
                control.display()
            );
        }
        assert!(
            Instant::now() < deadline,
            "UNAVAILABLE: native milestone {file} timed out; not acceptance"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn released(
    host: &wrix_state::host::Host,
    selection: &Selection,
    ticket: &wrix_state::execution::Ticket,
    root: &Path,
) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("client.json")).unwrap()).unwrap();
        if record["state"]["phase"] == "released" {
            assert!(
                wrix_state::execution::terminal(host, selection.context().unwrap().key(), ticket)
                    .unwrap()
                    .is_some()
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native retirement remains protected: {record:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn retire(
    host: &wrix_state::host::Host,
    selection: &Selection,
    control: &Path,
    name: &str,
    worker: &mut Worker,
) {
    fs::write(control.join(format!("{name}-exit")), b"exit").unwrap();
    assert!(worker.running.child().wait().unwrap().success());
    assert_eq!(
        worker.running.wait_for_retirement(BUDGET).unwrap(),
        wrix_sandbox::execution::Observation::Retired
    );
    assert!(
        wrix_state::execution::terminal(host, selection.context().unwrap().key(), &worker.ticket)
            .unwrap()
            .is_some()
    );
    // The guest conservatively retains a closing lease until all native work
    // and readers drain. Other live workers must not force early release.
}
pub(super) fn pool_record(
    host: &wrix_state::host::Host,
    selection: &Selection,
) -> serde_json::Value {
    let wrix_state::nix_store::Approval::Bound { repository, .. } =
        wrix_state::nix_store::inspect(selection).unwrap()
    else {
        panic!("native owner observation requires existing checked membership");
    };
    serde_json::from_slice(
        &fs::read(
            host.roots()
                .state()
                .join("control/nix")
                .join(repository.to_string())
                .join(system().as_str())
                .join("backend.json"),
        )
        .unwrap(),
    )
    .unwrap()
}
fn replay(
    platform: Platform,
    selection: &Selection,
    host: &wrix_state::host::Host,
    binding: &wrix_core::nix_store::binding::Binding,
) {
    let ticket = wrix_state::execution::reserve(host, selection.context().unwrap()).unwrap();
    let guard = Admission::acquire(selection, system()).unwrap();
    let intent = match platform {
        Platform::Apple => guard.request_apple_worker(ticket.clone(), binding.clone()),
        Platform::Krun => guard.request_krun_worker(ticket.clone(), binding.clone()),
        Platform::Podman => guard.request_podman_worker(ticket.clone(), binding.clone()),
    }
    .unwrap();
    let accepted = request::claim(intent.id()).unwrap();
    assert!(
        request::claim(intent.id()).is_err(),
        "native preparation intent replayed"
    );
    accepted.recheck().unwrap();
    wrix_state::execution::cancel(host, selection.context().unwrap(), &ticket).unwrap();
    assert!(accepted.recheck().is_err());
}

pub(super) fn exercise(
    harness: &Harness<'_>,
    daemon: &mut Running,
    binding: &wrix_core::nix_store::binding::Binding,
) {
    let Harness {
        platform,
        factory,
        selection,
        linked,
        peer,
        host,
        runtime,
        client,
    } = *harness;
    let control = selection.context().unwrap().caller().join("native-control");
    fs::create_dir(&control).unwrap();
    let archive = fs::File::create(control.join("unsigned.nar")).unwrap();
    let status = Command::new(std::env::var_os("WRIX_TEST_POOL_NIX_STORE").unwrap())
        .env_remove("NIX_REMOTE")
        .arg("--export")
        .arg(std::env::var_os("WRIX_TEST_POOL_UNSIGNED").unwrap())
        .stdout(Stdio::from(archive))
        .status()
        .unwrap();
    assert!(status.success());
    let outside = host.roots().state().join("native-world-readable-control");
    fs::write(&outside, b"readable-only-outside-worker").unwrap();
    fs::set_permissions(
        &outside,
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    fs::write(
        control.join("outside-path"),
        outside.as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    replay(platform, selection, host, binding);
    let started = Instant::now();
    let mut first = launch(
        platform, factory, selection, host, &control, "first", binding,
    );
    let mut second = launch(platform, factory, linked, host, &control, "second", binding);
    milestone(&control, "first-ready.json", &mut [&mut first, &mut second]);
    milestone(
        &control,
        "second-ready.json",
        &mut [&mut first, &mut second],
    );
    let first_ready = observation::read(&control.join("first-ready.json"));
    let second_ready = observation::read(&control.join("second-ready.json"));
    observation::independent(platform, &first_ready, &second_ready);
    let owner = pool_record(host, selection);
    let owner_id = owner["state"]["connection"]["target"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    observation::service_boundary(
        platform,
        &first.runtime,
        &owner_id,
        &first_ready,
        host,
        selection,
    );
    let mut peer_worker = launch(platform, factory, peer, host, &control, "peer", binding);
    milestone(
        &control,
        "peer-ready.json",
        &mut [&mut first, &mut second, &mut peer_worker],
    );
    let mut probe = boundary::Probe::start(&first.runtime, host, selection, &control);
    let before = probe.snapshot();
    fs::write(
        control.join("service-before.json"),
        serde_json::to_vec(&before).unwrap(),
    )
    .unwrap();
    fs::write(control.join("go"), b"go").unwrap();
    milestone(&control, "first-done.json", &mut [&mut first, &mut second]);
    milestone(&control, "second-done.json", &mut [&mut first, &mut second]);
    let first_done = observation::read(&control.join("first-done.json"));
    let second_done = observation::read(&control.join("second-done.json"));
    observation::workflows(&first_done, &second_done);
    fs::write(control.join("resources-go"), b"go").unwrap();
    milestone(
        &control,
        "first-resources.json",
        &mut [&mut first, &mut second],
    );
    let after = probe.snapshot();
    boundary::resources(&before, &after);
    fs::write(
        control.join("service-after.json"),
        serde_json::to_vec(&after).unwrap(),
    )
    .unwrap();
    probe.stop();
    super::super::maintenance::busy(host);
    let failed_gc = Command::new(env!("CARGO_BIN_EXE_wrix"))
        .current_dir(selection.context().unwrap().caller())
        .args(["service", "nix-store", "gc", "--wait", "0s"])
        .output()
        .unwrap();
    fs::write(control.join("live-gc.stdout"), &failed_gc.stdout).unwrap();
    fs::write(control.join("live-gc.stderr"), &failed_gc.stderr).unwrap();
    assert_eq!(failed_gc.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(failed_gc.stdout).unwrap(),
        format!(
            "{}: incomplete: maintenance access remained busy until its acquisition deadline\n",
            system().as_str()
        ),
        "native CLI collection did not report scoped contention; stderr: {}",
        String::from_utf8_lossy(&failed_gc.stderr),
    );
    assert_ne!(peer_worker.ticket, first.ticket);
    let first_ticket = first.ticket.clone();
    let first_root = first.root.clone();
    let broker_before = if platform == Platform::Apple {
        Some(fs::read(first.root.join("broker-consumed.json")).unwrap())
    } else {
        None
    };
    first.running.child().kill().unwrap();
    assert!(!first.running.child().wait().unwrap().success());
    drop(first.running);
    assert!(
        wrix_state::execution::terminal(host, selection.context().unwrap().key(), &first_ticket)
            .unwrap()
            .is_none()
    );
    wrix_state::nix_store::set(host, selection.context().unwrap(), false).unwrap();
    stop_controller(daemon);
    *daemon = controller(host);
    assert_eq!(
        runtime
            .block_on(client.request(Operation::InspectNixPool {}))
            .unwrap(),
        Reply::PoolInterface { version: 2 }
    );
    assert_eq!(
        pool_record(host, selection)["state"]["connection"]["target"]["id"],
        owner_id
    );
    if let Some(before) = broker_before {
        assert_eq!(
            fs::read(first_root.join("broker-consumed.json")).unwrap(),
            before,
            "broker start was replayed across controller restart"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(first_root.join("broker.json")).unwrap()
            )
            .unwrap()["worker"],
            first.native["worker"]
        );
    }
    fs::write(control.join("recheck"), b"recheck").unwrap();
    milestone(&control, "first-resumed.json", &mut [&mut second]);
    assert!(
        wrix_state::execution::terminal(host, selection.context().unwrap().key(), &first_ticket)
            .unwrap()
            .is_none()
    );
    super::super::maintenance::busy(host);
    wrix_state::nix_store::set(host, selection.context().unwrap(), true).unwrap();
    retire(host, linked, &control, "second", &mut second);
    let next_binding = super::super::seed::next_binding();
    assert_ne!(next_binding.digest(), binding.digest());
    let mut replacement = launch(
        platform,
        factory,
        selection,
        host,
        &control,
        "replacement",
        &next_binding,
    );
    milestone(&control, "replacement-done.json", &mut [&mut replacement]);
    let replacement_done = observation::read(&control.join("replacement-done.json"));
    observation::workflows(&first_done, &replacement_done);
    fs::write(control.join("first-exit"), b"exit").unwrap();
    retire(host, selection, &control, "replacement", &mut replacement);
    retire(host, peer, &control, "peer", &mut peer_worker);
    released(host, selection, &first_ticket, &first_root);
    released(host, linked, &second.ticket, &second.root);
    released(host, selection, &replacement.ticket, &replacement.root);
    released(host, peer, &peer_worker.ticket, &peer_worker.root);
    observation::report(
        platform,
        &control,
        &[first_done, second_done, replacement_done],
        started.elapsed(),
    );
}

pub(super) fn execute(runtime: &Command, arguments: &[&str]) -> std::process::Output {
    let output = recipe(runtime).args(arguments).output().unwrap();
    assert!(
        output.status.success(),
        "native observation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
