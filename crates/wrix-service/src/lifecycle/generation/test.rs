use std::os::unix::fs::PermissionsExt as _;

use super::*;

#[test]
fn retained_generation_uses_selected_runtime_instead_of_path() {
    const CHILD: &str = "WRIX_TEST_GENERATION_RUNTIME";
    if std::env::var_os(CHILD).is_some() {
        let selected = std::env::var("WRIX_CONTAINER_RUNTIME").unwrap();
        let gateway = plan().gateway;
        assert_eq!(gateway.runtime().binary, selected);
        assert_eq!(
            gateway.runtime().status_identifier("owner").unwrap(),
            RuntimeStatus::Running
        );
        // A Podman override must not reinterpret a recorded Apple generation.
        assert_eq!(runtime(RuntimeKind::Container).binary, "container");
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
