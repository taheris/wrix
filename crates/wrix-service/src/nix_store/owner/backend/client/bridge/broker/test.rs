use super::*;
use std::{
    num::NonZeroU16,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
};
fn fixture() -> (tempfile::TempDir, Record, Client) {
    let directory = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = directory.path().canonicalize().unwrap();
    create(&root, "key", b"execution-private-key").unwrap();
    create(&root, "known-hosts", b"pinned-server").unwrap();
    let identity = serde_json::json!({"name":"wrix-worker-original", "born":800_000_001.125});
    let process: Process =
        serde_json::from_value(serde_json::json!({"identity":identity,"pid":123,"domain":"gui"}))
            .unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    let worker: Binding = serde_json::from_value(serde_json::json!({
        "identity":identity,
        "configuration":{"id":"wrix-worker-original","creationDate":800_000_001.125},
        "image":{"index":digest,"reference":"localhost/worker:latest","architecture":"arm64",
            "executable":"/nix/store/00000000000000000000000000000000-helper/bin/wrix-apple-nix-bridge",
            "expected":digest,"seed":"/nix/store/00000000000000000000000000000000-seed"},
        "mounts":[{"Source":root.join("store"),"Destination":"/nix/store","RW":false},
            {"Source":root.join("worker"),"Destination":"/run/wrix/nix-pool","RW":false}]
    })).unwrap();
    let delivery = Delivery {
        plane: Plane::Apple,
        pool: wrix_core::nix_store::pool::Key {
            repository: "a".repeat(32).try_into().unwrap(),
            system: wrix_core::nix_store::pool::System::Aarch64Linux,
        },
        ticket: wrix_state::execution::Ticket::parse(&"b".repeat(64)).unwrap(),
        lease: "c".repeat(32).try_into().unwrap(),
        store: root.join("store"),
        key: root.join("key"),
        known_hosts: root.join("known-hosts"),
        port: NonZeroU16::new(23456).unwrap(),
    };
    let record = Record {
        version:1, epoch:"d".repeat(32).try_into().unwrap(), delivery:delivery.clone(),
        namespace:"e".repeat(64).try_into().unwrap(), generation:vec![7;32],
        tools:serde_json::from_value(serde_json::json!({"version":1,"root":"/nix/store/00000000000000000000000000000000-runtime","system":"aarch64-darwin"})).unwrap(),
        environment:BTreeMap::from([("HOME".into(), "/Users/example & user".into()),("LC_ALL".into(),"C".into())]),
        worker, process:process.clone(), key:hash(&root,"key").unwrap(), known_hosts:hash(&root,"known-hosts").unwrap(),
    };
    let client = Client {
        version: 1,
        plane: Plane::Apple,
        pool: delivery.pool,
        namespace: record.namespace.clone(),
        generation: record.generation.clone(),
        ticket: delivery.ticket,
        lease: delivery.lease,
        native_process: Some(process),
        broker_epoch: Some(record.epoch.clone()),
        port: Some(delivery.port),
        state: State::Detached,
    };
    (directory, record, client)
}
#[test]
fn durable_delivery_cannot_change_lease_port_key_store_generation_or_worker_process() {
    let (directory, record, client) = fixture();
    let root = directory.path();
    assert!(record.binding(root, &client).is_ok());
    let original = serde_json::to_value(&record).unwrap();
    for (pointer, value) in [
        ("/epoch", serde_json::json!("f".repeat(32))),
        ("/delivery/port", serde_json::json!(2222)),
        ("/delivery/lease", serde_json::json!("f".repeat(32))),
        (
            "/delivery/store",
            serde_json::to_value(PathBuf::from("/tmp/replaced")).unwrap(),
        ),
        ("/generation/0", serde_json::json!(9)),
        ("/process/pid", serde_json::json!(124)),
        ("/worker/identity/born", serde_json::json!(800_000_003.0)),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        match serde_json::from_value::<Record>(changed) {
            Ok(changed) => assert!(changed.binding(root, &client).is_err(), "{pointer}"),
            Err(error) => assert!(pointer == "/delivery/store", "{pointer}: {error}"),
        }
    }
    fs::write(root.join("key"), b"substituted").unwrap();
    assert!(record.binding(root, &client).is_err());
}
#[test]
fn recovery_loads_the_same_epoch_without_recreating_missing_or_consumed_brokers() {
    let (directory, record, mut client) = fixture();
    let root = directory.path();
    assert!(existing(root, &client).is_err());
    create(root, RECORD, &serde_json::to_vec(&record).unwrap()).unwrap();
    let recovered = existing(root, &client).unwrap().unwrap();
    assert_eq!(recovered.epoch, record.epoch);
    assert_eq!(recovered.delivery, record.delivery);
    assert_eq!(
        serde_json::to_vec(&recovered.worker).unwrap(),
        serde_json::to_vec(&record.worker).unwrap()
    );
    fs::remove_file(root.join(RECORD)).unwrap();
    assert!(existing(root, &client).is_err());
    client.broker_epoch = None;
    assert!(existing(root, &client).is_err());
    client.native_process = None;
    assert!(existing(root, &client).unwrap().is_none());
    create(
        root,
        "broker-consumed.json",
        &serde_json::to_vec(&record.epoch).unwrap(),
    )
    .unwrap();
    assert!(existing(root, &client).is_err());
    assert!(create(root, "broker-consumed.json", b"again").is_err());
}
#[test]
fn durable_records_reject_symlink_hardlink_and_unknown_field_substitution() {
    let (directory, record, _) = fixture();
    let root = directory.path();
    create(root, RECORD, &serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(read::<Record>(root, RECORD).is_ok());
    fs::hard_link(root.join(RECORD), root.join("alias")).unwrap();
    assert!(read::<Record>(root, RECORD).is_err());
    fs::remove_file(root.join("alias")).unwrap();
    fs::rename(root.join(RECORD), root.join("original")).unwrap();
    std::os::unix::fs::symlink(root.join("original"), root.join(RECORD)).unwrap();
    assert!(read::<Record>(root, RECORD).is_err());
    let mut changed = serde_json::to_value(&record).unwrap();
    changed["program"] = serde_json::json!("arbitrary");
    assert!(serde_json::from_value::<Record>(changed).is_err());
}
#[test]
fn launchd_plan_selects_only_the_frozen_helper_and_private_control_epoch_without_keepalive() {
    let (directory, record, _) = fixture();
    let plist = record.plist(directory.path()).unwrap();
    assert!(plist.contains("<key>KeepAlive</key><false/>"));
    assert!(plist.contains("<key>RunAtLoad</key><true/>"));
    assert!(plist.contains("/nix/store/00000000000000000000000000000000-runtime/bin/wrix-nix-worker-broker</string><string>--control</string>"));
    assert!(
        plist.contains("<string>--epoch</string><string>dddddddddddddddddddddddddddddddd</string>")
    );
    assert!(plist.contains("/Users/example &amp; user"));
    assert!(!plist.contains("23456"));
    assert!(!plist.contains("--program"));
    let runtime = record.runtime();
    assert_eq!(
        runtime.get_program(),
        "/nix/store/00000000000000000000000000000000-runtime/bin/container"
    );
    assert_eq!(runtime.get_current_dir(), Some(Path::new("/")));
    assert_eq!(environment(&runtime).unwrap(), record.environment);
    assert!(runtime.get_args().next().is_none());
}
#[test]
fn private_control_ancestry_rejects_aliases_traversal_and_nonprivate_ancestors() {
    let directory = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let state = directory.path().canonicalize().unwrap();
    let root = state.join("control/nix/repository/aarch64-linux/executions/ticket");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .unwrap();
    assert!(ancestry_under(&state, &root).is_ok());
    assert!(ancestry_under(&state, &root.join("../ticket")).is_err());
    assert!(ancestry_under(&state, Path::new("/outside/control")).is_err());
    fs::set_permissions(state.join("control/nix"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ancestry_under(&state, &root).is_err());
    fs::set_permissions(state.join("control/nix"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(state.join("control/nix"), state.join("control/retained")).unwrap();
    std::os::unix::fs::symlink(state.join("control/retained"), state.join("control/nix")).unwrap();
    assert!(ancestry_under(&state, &root).is_err());
}

#[test]
fn lost_or_failed_broker_readiness_keeps_the_original_intent_and_never_claims_completion() {
    let (directory, record, client) = fixture();
    let root = directory.path();
    create(root, RECORD, &serde_json::to_vec(&record).unwrap()).unwrap();
    let original = fs::read(root.join(RECORD)).unwrap();
    assert!(record.observe(root).is_err());
    publish(
        root,
        "broker-status.json",
        &Status::Failed {
            epoch: record.epoch.clone(),
            pid: 123,
        },
    )
    .unwrap();
    assert!(record.observe(root).is_err());
    assert!(existing(root, &client).unwrap().is_some());
    assert_eq!(fs::read(root.join(RECORD)).unwrap(), original);
    assert!(!root.join(PLIST).exists());
}

#[test]
fn private_arguments_reject_public_port_program_and_pool_selectors() {
    let parse = |args: &[&str]| arguments(args.iter().map(std::ffi::OsString::from));
    assert!(
        parse(&[
            "--control",
            "/private/execution",
            "--epoch",
            &"d".repeat(32)
        ])
        .is_ok()
    );
    for args in [
        vec!["--port", "2222"],
        vec!["--program", "container"],
        vec!["--pool", "peer"],
        vec!["--control", "/private", "--epoch", "../replay"],
    ] {
        assert!(parse(&args).is_err());
    }
}
#[test]
fn native_observation_requires_exact_owned_plist_program_and_unique_live_pid() {
    let root = Path::new("/private/execution");
    let program = std::ffi::OsStr::new("/nix/store/runtime/bin/wrix-nix-worker-broker");
    let text=b"gui/501/org.wrix.nix-worker.epoch = {\n path = /private/execution/broker.plist\n program = /nix/store/runtime/bin/wrix-nix-worker-broker\n pid = 123\n}\n";
    assert!(native(text, root, program, 123).is_ok());
    assert!(native(text, root, program, 124).is_err());
    assert!(native(text, Path::new("/other"), program, 123).is_err());
    assert!(native(text, root, "/tmp/substitute".as_ref(), 123).is_err());
    let mut duplicate = text.to_vec();
    duplicate.extend_from_slice(b" pid = 124\n");
    assert!(native(&duplicate, root, program, 123).is_err());
}
#[test]
fn transport_stop_requires_both_authoritative_retirement_and_native_service_release() {
    let (_, _, mut client) = fixture();
    for state in [State::Registering, State::Attaching, State::Detached] {
        client.state = state;
        for removal in [Removal::Bound, Removal::Removing, Removal::Retired] {
            assert!(!released(&client, removal).unwrap());
        }
    }
    client.state = State::Released;
    assert!(released(&client, Removal::Bound).is_err());
    assert!(released(&client, Removal::Removing).is_err());
    assert!(released(&client, Removal::Retired).unwrap());
}

#[test]
fn concurrent_start_ownership_is_exclusive_and_observer_drop_does_not_release_it() {
    let (directory, record, _) = fixture();
    let first = lock(directory.path()).unwrap();
    assert!(lock(directory.path()).is_err());
    let observer = Bridge {
        _epoch: record.epoch,
    };
    drop(observer);
    assert!(lock(directory.path()).is_err());
    FileExt::unlock(&first).unwrap();
    drop(first);
    assert!(lock(directory.path()).is_ok());
}
