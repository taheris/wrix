use super::super::test::{recorded_owner, target as podman_target};
use super::super::{Connection, LEDGER};
use super::*;

fn fixture() -> (tempfile::TempDir, Owner, Target, Value) {
    let mut wire = podman_target();
    wire["id"] = json!(format!("wrix-nix-{}", "b".repeat(32)));
    wire["attempt"]["software"]["tools"]["system"] = json!("aarch64-darwin");
    wire["attempt"]["software"]["environment"] = json!({});
    wire["attempt"]["software"]["reference"] = json!("localhost/pool:test");
    let target: Target = serde_json::from_value(wire).unwrap();
    let (directory, mut owner) = recorded_owner(State::Starting {
        attempt: target.attempt.clone(),
    });
    owner.record.pool.system = wrix_core::nix_store::pool::System::Aarch64Linux;
    let metadata = fs::metadata(owner.prepared.data_root()).unwrap();
    let config = json!({
        "id":target.id.0,
        "creationDate":"2026-06-10T01:02:03Z",
        "image":{"reference":"localhost/pool:test", "descriptor":{"digest":format!("sha256:{}", "c".repeat(64))}},
        "labels":labels(&owner, &target.attempt),
        "resources":{"cpus":2,"cpuOverhead":1,"memoryInBytes":4_294_967_296_u64},
        "mounts":[{"type":{"virtiofs":{}},"source":owner.prepared.data_root(),"destination":"/var/lib/wrix/nix-pool","options":["rw"]}],
        "publishedPorts":[{"hostAddress":"127.0.0.1","hostPort":2222,"containerPort":2222,"proto":"tcp","count":1}],
        "publishedSockets":[], "platform":{"os":"linux","architecture":"arm64"}, "sysctls":{},
        "runtimeHandler":"container-runtime-linux", "rosetta":false,"ssh":false,"virtualization":false,"readOnly":false,"useInit":false,
        "capAdd":["CAP_NET_ADMIN","CAP_SYS_ADMIN","CAP_SYS_RESOURCE"], "capDrop":[],
        "initProcess":{"executable":"/bin/wrix-nix-pool","arguments":[],"workingDirectory":"/","terminal":false,"user":{"id":{"uid":0,"gid":0}},"supplementalGroups":[],"rlimits":[],"environment":[format!("WRIX_NIX_POOL_DATA_OWNER={}:{}",metadata.uid(),metadata.gid())]}
    });
    (directory, owner, target, config)
}

fn check(owner: &Owner, target: &Target, config: &Value) -> Result<(), Error> {
    let metadata = fs::metadata(owner.prepared.data_root()).unwrap();
    validate(
        owner,
        &target.attempt,
        config,
        NonZeroU16::new(2222).unwrap(),
        metadata.uid(),
        metadata.gid(),
    )
}

#[test]
fn actual_managed_container_schema_preserves_creation_name_and_birth() {
    let (_directory, owner, target, config) = fixture();
    let snapshot: Snapshot = serde_json::from_value(
        json!({"id":target.id.0,"configuration":config,"status":{"state":"running","networks":[]}}),
    )
    .unwrap();
    check(&owner, &target, &snapshot.configuration).unwrap();
    assert!(matches!(snapshot.status.state, RuntimeState::Running));
    assert_eq!(
        snapshot.configuration["creationDate"],
        "2026-06-10T01:02:03Z"
    );
    assert!(
        serde_json::from_value::<Snapshot>(json!({"Id":"a".repeat(64),"State":{"Running":true}}))
            .is_err()
    );
}

#[test]
fn native_census_accepts_unrelated_apple_names_without_adopting_them() {
    let (_directory, _owner, target, config) = fixture();
    let entries: Vec<Snapshot> = serde_json::from_value(json!([
        {"id":"other-project", "configuration":{"id":"other-project"}, "status":{"state":"running"}},
        {"id":target.id.0, "configuration":config, "status":{"state":"stopped"}}
    ])).unwrap();
    assert_eq!(entries.len(), 2);
    assert_ne!(entries[0].id.0, target.id.0);
    assert_eq!(entries[1].id.0, target.id.0);
}

#[test]
fn identity_mount_resource_and_endpoint_drift_are_rejected() {
    let (_directory, owner, target, config) = fixture();
    check(&owner, &target, &config).unwrap();
    for (pointer, value) in [
        ("/id", json!("wrix-nix-00000000000000000000000000000000")),
        ("/creationDate", json!(0)),
        (
            "/image/descriptor/digest",
            json!(format!("sha256:{}", "d".repeat(64))),
        ),
        ("/mounts/0/type", json!("bind")),
        ("/mounts/0/source", json!("/peer")),
        ("/mounts/0/options", json!(["ro"])),
        ("/resources/cpus", json!(4)),
        ("/resources/memoryInBytes", json!(8_589_934_592_u64)),
        ("/publishedPorts/0/hostAddress", json!("0.0.0.0")),
        ("/publishedPorts/0/hostPort", json!(2223)),
        ("/initProcess/user", json!({"id":{"uid":501,"gid":20}})),
        (
            "/initProcess/environment",
            json!([
                "WRIX_NIX_POOL_DATA_OWNER=0:0",
                "WRIX_NIX_POOL_DATA_OWNER=501:20"
            ]),
        ),
    ] {
        let mut changed = config.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(check(&owner, &target, &changed).is_err(), "{pointer}");
    }
}

#[test]
fn apple_image_variant_config_is_not_the_native_descriptor_digest() {
    let native = "a".repeat(64);
    let config =
        json!({"os":"linux","architecture":"arm64","config":{"Labels":{"wrix.managed":"true"}}});
    let bytes = serde_json::to_vec(&json!([{"id":native,"configuration":{"descriptor":{"digest":format!("sha256:{native}")}},"variants":[{"platform":{"os":"linux","architecture":"arm64"},"digest":format!("sha256:{}","b".repeat(64)),"config":config}]}])).unwrap();
    assert_eq!(
        parse_image(&bytes, wrix_core::nix_store::pool::System::Aarch64Linux).unwrap(),
        config
    );
    assert!(parse_image(&bytes, wrix_core::nix_store::pool::System::X86_64Linux).is_err());
}

#[test]
fn missing_failed_and_malformed_attach_receipts_never_prove_release() {
    let (_directory, owner, target, _) = fixture();
    assert!(clean_exit(&owner, &target).is_err());
    let root = &owner.prepared.reservation.paths.control;
    for content in [b"127\n".as_slice(), b"00\n", b"0\nextra", b"-1\n"] {
        let mut file = private_file(root, &outcome_name(&target), true).unwrap();
        file.write_all(content).unwrap();
        drop(file);
        assert!(clean_exit(&owner, &target).is_err());
        fs::remove_file(root.join(outcome_name(&target))).unwrap();
    }
    let mut file = private_file(root, &outcome_name(&target), true).unwrap();
    file.write_all(b"0\n").unwrap();
    assert_eq!(outcome(&owner, &target).unwrap(), Some(0));
    assert!(clean_exit(&owner, &target).is_err());
}

#[test]
fn durable_binding_keeps_birth_endpoint_and_host_data_identity() {
    let (_directory, _owner, mut target, config) = fixture();
    target.apple = Some(Binding {
        configuration: config.clone(),
        port: NonZeroU16::new(2222).unwrap(),
        uid: 501,
        gid: 20,
        process: None,
    });
    let decoded: Target = serde_json::from_slice(&serde_json::to_vec(&target).unwrap()).unwrap();
    let binding = decoded.apple.unwrap();
    assert_eq!(binding.configuration, config);
    assert_eq!((binding.uid, binding.gid), (501, 20));
    assert!(binding.matches_port(NonZeroU16::new(2222).unwrap()));
    assert!(!binding.matches_port(NonZeroU16::new(2223).unwrap()));
}

#[test]
fn failed_apple_release_retry_cannot_acquire_candidate_or_rewrite_ownership() {
    let (_directory, mut owner, mut target, config) = fixture();
    let metadata = fs::metadata(owner.prepared.data_root()).unwrap();
    let port = NonZeroU16::new(2222).unwrap();
    target.apple = Some(Binding {
        configuration: config,
        port,
        uid: metadata.uid(),
        gid: metadata.gid(),
        process: None,
    });
    let connection = Connection {
        target: target.clone(),
        port,
        host_key: super::super::test::key().try_into().unwrap(),
    };
    owner.record.state = State::Released { connection };
    owner
        .record
        .publish(&owner.prepared.reservation.paths.control)
        .unwrap();
    let root = owner.prepared.reservation.paths.control.clone();
    let before = fs::read(root.join(LEDGER)).unwrap();
    private_file(owner.prepared.data_root(), "released", true)
        .unwrap()
        .write_all(b"1\n")
        .unwrap();
    for receipt in [None, Some(b"127\n".as_slice())] {
        if let Some(bytes) = receipt {
            private_file(&root, &outcome_name(&target), true)
                .unwrap()
                .write_all(bytes)
                .unwrap();
        }
        for _retry in 0..2 {
            assert!(matches!(
                owner.update(
                    &target.attempt.software.candidate,
                    &target.attempt.software.tools
                ),
                Err(Error::Retained)
            ));
            assert!(matches!(owner.resume(), Err(Error::Retained)));
            assert_eq!(fs::read(root.join(LEDGER)).unwrap(), before);
            assert!(!root.join("software-roots").exists());
        }
    }
}

#[test]
fn native_birth_parser_rejects_deferred_numeric_and_invalid_calendar_dates() {
    for valid in ["2024-02-29T23:59:59Z", "2026-06-10T01:02:03Z"] {
        birth_date(valid).unwrap();
    }
    for invalid in [
        "2023-02-29T23:59:59Z",
        "2026-13-10T01:02:03Z",
        "2026-06-10T24:02:03Z",
        "2026-06-10T01:02:03Zextra",
        "803699323.0",
    ] {
        assert!(birth_date(invalid).is_err());
    }
}
