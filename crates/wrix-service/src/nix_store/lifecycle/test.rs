use super::*;
use serde_json::json;

fn target() -> Package {
    serde_json::from_value(json!({
        "controller":"/nix/store/00000000000000000000000000000000-controller",
        "runtime":"/nix/store/00000000000000000000000000000000-runtime",
        "image":"/nix/store/00000000000000000000000000000000-image.drv"
    }))
    .unwrap()
}
fn update() -> Intent {
    serde_json::from_value(json!({
        "operation":"update",
        "candidate":{
            "schema":1,"image":"localhost/pool:test","source_kind":"nix-descriptor",
            "source":{"path":"/source","derivation":"/source.drv"},
            "digest":{"path":"/digest","derivation":"/digest.drv"}
        },
        "tools":{"version":1,"root":"/nix/store/00000000000000000000000000000000-runtime","system":"x86_64-linux"}
    })).unwrap()
}
fn pool() -> Key {
    Key {
        repository: "a".repeat(32).try_into().unwrap(),
        system: wrix_core::nix_store::pool::System::X86_64Linux,
    }
}
fn marker(intent: Intent) -> Marker {
    Marker {
        version: 1,
        generation: vec![7; 32],
        deployment: "b".repeat(32).try_into().unwrap(),
        target: target(),
        intent,
        entries: vec![Entry::Serving(pool())],
    }
}

#[test]
fn legacy_repair_receipts_preserve_recorded_software_authority() {
    let value = serde_json::to_value(marker(Intent::Repair)).unwrap();
    assert!(value.get("intent").is_none());
    let recovered: Marker = serde_json::from_value(value).unwrap();
    assert_eq!(recovered.intent, Intent::Repair);
    recovered.check_intent(&target(), &Intent::Repair).unwrap();
    assert!(matches!(
        recovered.check_intent(&target(), &update()),
        Err(Error::Conflict)
    ));
}

#[test]
fn interrupted_update_cannot_be_reinterpreted_as_repair() {
    let original = marker(update());
    let recovered: Marker =
        serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(original, recovered);
    recovered.check_intent(&target(), &update()).unwrap();
    assert!(matches!(
        recovered.check_intent(&target(), &Intent::Repair),
        Err(Error::Conflict)
    ));
}

#[test]
fn interrupted_update_requires_the_exact_pool_candidate() {
    let original = marker(update());
    let mut value = serde_json::to_value(update()).unwrap();
    value["candidate"]["image"] = json!("localhost/pool:other");
    let changed = serde_json::from_value(value).unwrap();
    assert!(matches!(
        original.check_intent(&target(), &changed),
        Err(Error::Conflict)
    ));
}

#[test]
fn interrupted_update_requires_the_exact_host_runtime() {
    let original = marker(update());
    let mut value = serde_json::to_value(update()).unwrap();
    value["tools"]["root"] = json!("/nix/store/11111111111111111111111111111111-other-runtime");
    let changed = serde_json::from_value(value).unwrap();
    assert!(matches!(
        original.check_intent(&target(), &changed),
        Err(Error::Conflict)
    ));
}

#[test]
fn update_cannot_borrow_another_controller_packages_runtime() {
    let Intent::Update { tools, .. } = update() else {
        panic!("expected update fixture")
    };
    assert!(tools.belongs_to(&target()));
    let mut value = serde_json::to_value(target()).unwrap();
    value["runtime"] = json!("/nix/store/11111111111111111111111111111111-other-runtime");
    assert!(!tools.belongs_to(&serde_json::from_value(value).unwrap()));
}

#[test]
fn update_activates_provisioned_stopped_pools_but_repair_preserves_them() {
    assert_eq!(
        Entry::capture(&pool(), MaintenanceState::Released, &Intent::Repair).unwrap(),
        Entry::Released(pool())
    );
    assert_eq!(
        Entry::capture(&pool(), MaintenanceState::Released, &update()).unwrap(),
        Entry::Serving(pool())
    );
}

#[test]
fn fresh_maintenance_cannot_capture_an_uncertain_stop_as_release() {
    for intent in [Intent::Repair, update()] {
        assert!(matches!(
            Entry::capture(&pool(), MaintenanceState::Stopping, &intent),
            Err(Error::Invalid)
        ));
    }
}
