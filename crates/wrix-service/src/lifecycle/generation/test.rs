use std::os::unix::fs::PermissionsExt as _;

use super::*;

#[test]
fn explicit_cold_shutdown_retires_a_missing_obsolete_gateway_not_the_current_one() {
    const CHILD: &str = "WRIX_TEST_COLD_GATEWAY_RETIREMENT";
    if let Some(root) = std::env::var_os(CHILD) {
        let mut plan = plan();
        plan.obsolete.push(plan.gateway.clone());
        plan.gateway.generation = Generation::new(NonZeroU64::new(2).unwrap());
        plan.gateway.name = "wrix-gateway-2".to_owned().try_into().unwrap();
        stop_gateways(&plan, Path::new(&root)).unwrap();
        assert!(
            plan.obsolete[0]
                .retirement(Path::new(&root))
                .unwrap()
                .is_some()
        );
        assert!(plan.gateway.retirement(Path::new(&root)).unwrap().is_none());
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("podman");
    std::fs::write(&runtime, "#!/bin/bash\nset -euo pipefail\n[[ \"$1\" == inspect ]] || exit 64\nprintf 'Error: no such container\\n' >&2\nexit 125\n").unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lifecycle::generation::test::explicit_cold_shutdown_retires_a_missing_obsolete_gateway_not_the_current_one", "--nocapture"])
        .env(CHILD, root.path())
        .env("WRIX_CONTAINER_RUNTIME", runtime)
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn apple_endpoint_planning_rejects_unpublished_or_host_only_routes() {
    use crate::{control::Reply, route::Role};
    let ports = [23001, 21001, 31001, 31002, 23002].map(|port| NonZeroU16::new(port).unwrap());
    let guest = Ipv4Addr::new(10, 83, 0, 1);
    for (role, port) in Role::ALL.into_iter().zip(ports) {
        let endpoint = published_guest(
            Reply::CoreInterface {
                version: 2,
                ports,
                guest: Some(guest),
            },
            role,
        )
        .unwrap();
        assert_eq!(endpoint, SocketAddrV4::new(guest, port.get()));
    }
    for (version, guest) in [
        (1, Some(guest)),
        (3, Some(guest)),
        (2, None),
        (2, Some(Ipv4Addr::LOCALHOST)),
        (2, Some(Ipv4Addr::UNSPECIFIED)),
        (2, Some(Ipv4Addr::new(198, 51, 100, 7))),
    ] {
        assert!(
            published_guest(
                Reply::CoreInterface {
                    version,
                    ports,
                    guest
                },
                Role::Sql
            )
            .is_err()
        );
    }
    assert!(published_guest(Reply::Applied, Role::Sql).is_err());
}

#[test]
fn gateway_port_pair_is_distinct() {
    let [data, management] = available().unwrap();
    assert_ne!(data, management);
}

#[test]
fn gateway_port_selection_keeps_first_listener_reserved() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let mut first = Some(listener);
    let mut attempts = 0;
    let result = available_with(|| {
        attempts += 1;
        first.take().map_or_else(|| TcpListener::bind(address), Ok)
    });
    assert_eq!(attempts, 2);
    assert!(
        matches!(result, Err(Error::Io { source }) if source.kind() == io::ErrorKind::AddrInUse)
    );
}

#[test]
fn retained_generation_uses_selected_runtime_instead_of_path() {
    const CHILD: &str = "WRIX_TEST_GENERATION_RUNTIME";
    if std::env::var_os(CHILD).is_some() {
        let selected = std::env::var("WRIX_CONTAINER_RUNTIME").unwrap();
        let gateway = plan().gateway;
        assert_eq!(gateway.runtime().unwrap().binary, selected);
        assert_eq!(
            gateway
                .runtime()
                .unwrap()
                .status_identifier("owner")
                .unwrap(),
            RuntimeStatus::Running
        );
        // A Podman override must not reinterpret a recorded Apple generation.
        assert!(runtime(RuntimeKind::Container).is_err());
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let selected = root.path().join("selected-podman");
    let ambient = root.path().join("podman");
    for (path, body) in [
        (&selected, "printf 'true\\n'"),
        (
            &ambient,
            "printf 'wrong runtime: permission denied creating /var/lib/containers/storage/libpod\\n' >&2; exit 125",
        ),
    ] {
        std::fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "lifecycle::generation::test::retained_generation_uses_selected_runtime_instead_of_path",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("WRIX_CONTAINER_RUNTIME", selected)
        .env("PATH", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn plan() -> Plan {
    let recipe: owner::Recipe = serde_json::from_value(serde_json::json!({
        "binding": {
            "schema": 1,
            "image": "localhost/wrix-service:selected",
            "source_kind": "nix-descriptor",
            "source": {"path": "/nix/store/source", "derivation": "/nix/store/source.drv"},
            "digest": {"path": "/nix/store/digest/value", "derivation": "/nix/store/digest.drv"}
        },
        "image_id": format!("sha256:{}", "a".repeat(64)),
        "config_digest": format!("sha256:{}", "b".repeat(64))
    }))
    .unwrap();
    let generation = Generation::new(NonZeroU64::MIN);
    Plan {
        version: 1,
        deployment: "a".repeat(32).try_into().unwrap(),
        gateway: Gateway {
            generation,
            runtime: RuntimeKind::Podman,
            name: "wrix-gateway-1".to_owned().try_into().unwrap(),
            recipe: recipe.clone(),
            data: NonZeroU16::new(31000).unwrap(),
            management: NonZeroU16::new(31001).unwrap(),
            key: Secret::generate().unwrap(),
        },
        backend: Backend {
            generation,
            runtime: RuntimeKind::Podman,
            name: "wrix-host-service".to_owned().try_into().unwrap(),
            recipe,
            sql: NonZeroU16::new(31002).unwrap(),
            cache: NonZeroU16::new(31003).unwrap(),
            compiler: NonZeroU16::new(31004).unwrap(),
            sync: NonZeroU16::new(31005).unwrap(),
        },
        backend_open: true,
        obsolete: Vec::new(),
        preparing: None,
    }
}

fn status(plan: &Plan) -> Status {
    Status {
        version: 1,
        epoch: [1; 32],
        gateway: plan.gateway.generation,
        backend: plan.backend.generation,
        backend_open: plan.backend_open,
        state: State::Activated,
        retiring: 0,
        plan_digest: plan.digest().unwrap(),
    }
}

#[test]
fn readiness_for_an_old_recipe_cannot_acknowledge_replacement_in_the_same_generation() {
    let original = plan();
    let observed = status(&original);
    assert!(observed.matches(&original).unwrap());
    let mut replacement = original;
    replacement.backend.recipe.config_digest =
        wrix_sandbox::image::Digest::parse(&format!("sha256:{}", "c".repeat(64))).unwrap();
    assert!(!observed.matches(&replacement).unwrap());
}

#[test]
fn readiness_for_old_ports_cannot_acknowledge_a_changed_route() {
    let mut replacement = plan();
    let observed = status(&replacement);
    replacement.backend.sql = NonZeroU16::new(32000).unwrap();
    assert!(!observed.matches(&replacement).unwrap());
}

#[test]
fn failed_attachment_never_acknowledges_an_unchanged_plan() {
    let plan = plan();
    let mut observed = status(&plan);
    observed.state = State::Failed;
    assert!(!observed.matches(&plan).unwrap());
}
