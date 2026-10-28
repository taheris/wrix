use super::*;

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
