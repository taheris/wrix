//! Unclean daemon death is fenced recovery, not permission to replay a serving owner or discard roots.
use super::*;
use std::os::unix::fs::MetadataExt;

#[test]
#[ignore = "UNRUN: destructive native Apple daemon fault; deliberately retains fenced fixture storage and software roots"]
fn apple_native_daemon_death_retains_roots_and_refuses_replay() {
    run_case(
        Platform::Apple,
        "nix_store::admission::request::native::platform::fault::apple_native_daemon_death_retains_roots_and_refuses_replay",
        exercise,
        true,
    );
}
#[test]
#[ignore = "UNRUN: destructive native krun daemon fault; deliberately retains fenced fixture storage and software roots"]
fn krun_native_daemon_death_retains_roots_and_refuses_replay() {
    run_case(
        Platform::Krun,
        "nix_store::admission::request::native::platform::fault::krun_native_daemon_death_retains_roots_and_refuses_replay",
        exercise,
        true,
    );
}
fn refuses_replay(local: &Controller, selection: &Selection) {
    use wrix_service::nix_store::owner::backend::Error;
    let deadline = Instant::now() + BUDGET;
    loop {
        match local.ensure_nix_pool(selection, system()) {
            Err(wrix_service::control::Error::PoolOwner(Error::Busy)) => {
                assert!(
                    Instant::now() < deadline,
                    "owner lock contention is not recovery-fence evidence"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(wrix_service::control::Error::PoolOwner(
                Error::Retained | Error::Invalid | Error::Unavailable,
            )) => break,
            Err(error) => panic!("recovery refused for an unrelated reason: {error}"),
            Ok(_) => panic!("ordinary restart replayed an unclean native owner"),
        }
    }
}
fn roots(directory: &Path) -> std::collections::BTreeMap<PathBuf, PathBuf> {
    let mut roots = std::collections::BTreeMap::new();
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            roots.extend(self::roots(&entry.path()));
        } else {
            roots.insert(entry.path(), fs::read_link(entry.path()).unwrap());
        }
    }
    roots
}
fn exercise(platform: Platform, caller: &Path) {
    let selection = Selection::inspect(caller).unwrap();
    let host = wrix_state::host::Host::discover().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut daemon = controller(&host);
    let client = Client::open(&host.roots().state().join("control/authority")).unwrap();
    assert_eq!(
        runtime
            .block_on(client.request(Operation::Register {
                namespace: Namespace::from(selection.context().unwrap().key()),
                policy: Policy {
                    dolt: Access::Denied,
                    private_cache_read: Access::Denied,
                    common_cache_read: Access::Denied,
                    egress: Access::Denied,
                    compiler: Access::Denied
                },
            }))
            .unwrap(),
        Reply::Applied
    );
    let factory = Connected::new(client).unwrap();
    platform.admit(&factory);
    let binding = super::super::seed::binding();
    let control = caller.join("native-fault");
    fs::create_dir(&control).unwrap();
    let mut worker = worker::launch(
        platform, &factory, &selection, &host, &control, "fault", &binding,
    );
    worker::milestone(&control, "fault-ready.json", &mut [&mut worker]);
    let record = worker::pool_record(&host, &selection);
    let pool = Admission::acquire(&selection, system())
        .unwrap()
        .pool()
        .clone();
    let root = worker.root.parent().unwrap().parent().unwrap().to_owned();
    let software_roots = roots(&root.join("software-roots"));
    assert!(!software_roots.is_empty());
    let disk = host
        .roots()
        .state()
        .join("service-data/nix")
        .join(pool.repository.to_string())
        .join(pool.system.as_str())
        .join("store.ext4");
    let metadata = fs::metadata(&disk).unwrap();
    let disk_identity = (metadata.dev(), metadata.ino(), metadata.len());
    assert_eq!(metadata.len(), 8_589_934_592);
    let id = record["state"]["connection"]["target"]["id"]
        .as_str()
        .unwrap();
    stop_controller(&mut daemon);
    let (local, mut owner) = local_owner(&runtime, &host, &selection);
    assert!(matches!(
        owner.inspect().unwrap(),
        wrix_service::nix_store::native::Reply::Ready { .. }
    ));
    let signal = boundary::kill_daemon(&worker.runtime, id, &binding);
    let deadline = Instant::now() + BUDGET;
    loop {
        let mut command = worker::recipe(&worker.runtime);
        let output = match platform {
            Platform::Apple => command.args(["inspect", id]).output().unwrap(),
            Platform::Krun | Platform::Podman => command
                .args(["inspect", "--type", "container", id])
                .output()
                .unwrap(),
        };
        assert!(
            output.status.success(),
            "native ownership disappeared instead of retaining uncertain stopped state"
        );
        let values: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(values.len(), 1);
        let stopped = match platform {
            Platform::Apple => values[0]["status"]["state"] == "stopped",
            Platform::Krun | Platform::Podman => {
                values[0]["State"]["Status"] == "exited" && values[0]["State"]["Running"] == false
            }
        };
        if stopped {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual service VM did not fail-stop after daemon death"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !disk.parent().unwrap().join("released").exists(),
        "unclean daemon death forged a clean release certificate"
    );
    assert!(
        wrix_state::execution::terminal(&host, selection.context().unwrap().key(), &worker.ticket)
            .unwrap()
            .is_none()
    );
    fs::write(control.join("fault-recheck"), b"check").unwrap();
    worker::milestone(&control, "fault-disconnected.json", &mut [&mut worker]);
    assert!(
        matches!(
            owner.resume(),
            Err(wrix_service::nix_store::owner::backend::Error::Retained)
        ),
        "explicit resume must require the missing clean native release"
    );
    drop(owner);
    refuses_replay(&local, &selection);
    assert_eq!(
        worker::pool_record(&host, &selection)["state"]["connection"]["target"]["id"],
        id
    );
    assert_eq!(roots(&root.join("software-roots")), software_roots);
    let metadata = fs::metadata(&disk).unwrap();
    assert_eq!(
        (metadata.dev(), metadata.ino(), metadata.len()),
        disk_identity
    );
    let client: serde_json::Value =
        serde_json::from_slice(&fs::read(worker.root.join("client.json")).unwrap()).unwrap();
    assert_ne!(
        client["state"]["phase"], "released",
        "lost native service work released a live execution"
    );
    fs::write(control.join("fault-exit"), b"exit").unwrap();
    assert!(worker.running.child().wait().unwrap().success());
    assert_eq!(
        worker.running.wait_for_retirement(BUDGET).unwrap(),
        wrix_sandbox::execution::Observation::Retired
    );
    refuses_replay(&local, &selection);
    local.close().unwrap();
    fs::write(caller.parent().unwrap().join("native-measurements.json"), serde_json::to_vec_pretty(&serde_json::json!({"boundary":"isolated daemon-loss negative control; NOT public dispatch acceptance","native_signal":signal,"retained_native_owner":id,"disk_identity":disk_identity,"software_roots_retained":software_roots.len()})).unwrap()).unwrap();
}
