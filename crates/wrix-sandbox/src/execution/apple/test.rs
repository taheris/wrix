use super::*;
#[test]
fn native_creation_date_uses_actual_iso8601_and_rejects_invalid_dates() {
    assert_eq!(
        birth(&serde_json::json!("2026-05-09T06:13:21Z")).unwrap(),
        800_000_001.0
    );
    assert_eq!(
        birth(&serde_json::json!("2026-05-09T07:13:21+01:00")).unwrap(),
        800_000_001.0
    );
    for invalid in ["2026-02-30T00:00:00Z", "2026-05-09", "800000001", ""] {
        assert!(birth(&serde_json::json!(invalid)).is_err());
    }
}

fn resource(state: &str, started: Option<f64>) -> Value {
    serde_json::json!([{
        "id":"wrix-worker-actual-native-name",
        "configuration": {
            "id":"wrix-worker-actual-native-name", "creationDate": "2026-05-09T06:13:21.125Z",
            "image":{"reference":"localhost/worker:latest","descriptor":{"digest":format!("sha256:{}", "a".repeat(64)),"mediaType":"application/vnd.oci.image.index.v1+json","size":512}},
            "mounts":[{"type":{"virtiofs":{}},"source":"/selected/projection","destination":"/nix/store","options":["ro"]}],
            "publishedPorts":[],"publishedSockets":[],"labels":{},"sysctls":{},"networks":[],"rosetta":false,
            "initProcess":{"executable":"/network-bootstrap.sh","arguments":[],"environment":[],"workingDirectory":"/","terminal":false,"user":{"id":{"uid":0,"gid":0}},"supplementalGroups":[]},
            "platform":{"os":"linux","architecture":"arm64"},"resources":{"cpus":2,"memoryInBytes":4_294_967_296_u64},
            "runtimeHandler":"container-runtime-linux","virtualization":false,"ssh":false,"readOnly":false,"useInit":false,
            "capAdd":["CAP_NET_ADMIN"],"capDrop":[]
        },
        "status":{"state":state,"networks":[],"startedDate":started}
    }])
}
fn snapshot(value: &Value) -> Snapshot {
    parse(
        &serde_json::to_vec(value).unwrap(),
        "wrix-worker-actual-native-name",
    )
    .unwrap()
}
#[test]
fn native_projection_requires_the_exact_readonly_views_without_extra_capabilities() {
    let mounts = vec![
        ProjectionMount {
            source: "/selected/execution/store".into(),
            destination: "/nix/store".into(),
            writable: false,
        },
        ProjectionMount {
            source: "/selected/execution/worker".into(),
            destination: "/run/wrix/nix-pool".into(),
            writable: false,
        },
        ProjectionMount {
            source: "/selected/execution/worker/state".into(),
            destination: "/nix/var/nix".into(),
            writable: false,
        },
    ];
    let mut config = snapshot(&resource("stopped", None)).configuration;
    config["mounts"] = serde_json::json!([
        {"type":{"virtiofs":{}},"source":"/selected/execution/store","destination":"/nix/store","options":["ro"]},
        {"type":{"virtiofs":{}},"source":"/selected/execution/worker","destination":"/run/wrix/nix-pool","options":["ro"]},
        {"type":{"virtiofs":{}},"source":"/selected/execution/worker/state","destination":"/nix/var/nix","options":["ro"]}
    ]);
    let index = crate::image::Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap();
    assert!(verify_projection(&config, &index, "arm64", &mounts).is_ok());
    for (pointer, replacement) in [
        ("/mounts/0/options", serde_json::json!(["rw"])),
        ("/mounts/1/source", serde_json::json!("/state/db")),
        ("/mounts/2/source", serde_json::json!("/state/db")),
        ("/mounts/2/options", serde_json::json!(["rw"])),
        ("/mounts/2/destination", serde_json::json!("/nix/var")),
        (
            "/image/descriptor/digest",
            serde_json::json!(format!("sha256:{}", "b".repeat(64))),
        ),
        (
            "/capAdd",
            serde_json::json!(["CAP_NET_ADMIN", "CAP_SYS_ADMIN"]),
        ),
        ("/platform/architecture", serde_json::json!("amd64")),
    ] {
        let mut changed = config.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert!(verify_projection(&changed, &index, "arm64", &mounts).is_err());
    }
    let mut changed = config;
    changed["mounts"].as_array_mut().unwrap().push(serde_json::json!({"type":{"virtiofs":{}},"source":"/peer","destination":"/nix/store/peer","options":["ro"]}));
    assert!(verify_projection(&changed, &index, "arm64", &mounts).is_err());
}
#[test]
fn native_resource_schema_uses_creation_bound_name_not_a_podman_id() {
    let object = snapshot(&resource("running", Some(800_000_002.0)));
    assert_eq!(
        object.identity().unwrap().name(),
        "wrix-worker-actual-native-name"
    );
    assert_eq!(object.identity().unwrap().born(), 800_000_001.125);
    assert!(parse(br#"[{"Id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","State":{"Status":"exited"}}]"#, "wrix-worker-actual-native-name").is_err());
    let mut value = resource("running", Some(800_000_002.0));
    value[0]["configuration"]["creationDate"] = Value::Null;
    assert!(
        parse(
            &serde_json::to_vec(&value).unwrap(),
            "wrix-worker-actual-native-name"
        )
        .is_err()
    );
}
#[test]
fn only_exact_native_stopped_after_start_can_begin_removal() {
    let identity = snapshot(&resource("running", Some(800_000_002.0)))
        .identity()
        .unwrap();
    for (state, started, expected) in [
        ("running", Some(800_000_002.0), false),
        ("stopping", Some(800_000_002.0), false),
        ("unknown", Some(800_000_002.0), false),
        ("stopped", None, false),
        ("stopped", Some(0.0), false),
        ("stopped", Some(800_000_002.0), true),
    ] {
        assert_eq!(
            stopped(&snapshot(&resource(state, started)), &identity).unwrap(),
            expected
        );
    }
    let mut replaced = resource("stopped", Some(800_000_002.0));
    replaced[0]["configuration"]["creationDate"] = serde_json::json!(800_000_003.0);
    assert!(stopped(&snapshot(&replaced), &identity).is_err());
}
#[test]
fn duplicate_or_mismatched_native_resources_never_supply_evidence() {
    let mut value = resource("stopped", Some(800_000_002.0));
    let duplicate = value[0].clone();
    value.as_array_mut().unwrap().push(duplicate);
    assert!(
        parse(
            &serde_json::to_vec(&value).unwrap(),
            "wrix-worker-actual-native-name"
        )
        .is_err()
    );
    let mut value = resource("stopped", Some(800_000_002.0));
    value[0]["configuration"]["id"] = serde_json::json!("substituted");
    assert!(
        parse(
            &serde_json::to_vec(&value).unwrap(),
            "wrix-worker-actual-native-name"
        )
        .is_err()
    );
}
