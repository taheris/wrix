use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;

fn key() -> Key {
    let mut wire = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
    wire.extend([7_u8; 32]);
    format!("ssh-ed25519 {}", STANDARD.encode(wire))
        .try_into()
        .unwrap()
}

#[test]
fn legacy_startup_does_not_adopt_a_preexisting_disk() {
    let configuration: Configuration =
        serde_json::from_value(json!({"version": 1, "admin_key": key()})).unwrap();
    assert_eq!(configuration.storage(), Storage::Allocate);
    assert!(
        serde_json::to_value(configuration)
            .unwrap()
            .get("storage")
            .is_none()
    );
}

#[test]
fn host_preallocation_has_an_explicit_versioned_startup_contract() {
    let bytes = serde_json::to_vec(&Configuration::preallocated(key())).unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["version"], 2);
    assert_eq!(wire["storage"], "preallocated");
    let configuration: Configuration = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(configuration.storage(), Storage::Preallocated);
}

#[test]
fn retained_storage_requires_explicit_resume_version() {
    let wire = json!({"version": 3, "admin_key": key(), "storage": "resume"});
    let configuration: Configuration = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(configuration.storage(), Storage::Resume);
    assert_eq!(serde_json::to_value(configuration).unwrap(), wire);
    for version in [1, 2, 4] {
        assert!(
            serde_json::from_value::<Configuration>(
                json!({"version": version, "admin_key": key(), "storage": "resume"})
            )
            .is_err()
        );
    }
}

#[test]
fn explicit_upgrade_binds_the_previous_immutable_software_and_administration() {
    let previous: StorePath = "/nix/store/00000000000000000000000000000000-bootstrap"
        .to_owned()
        .try_into()
        .unwrap();
    let configuration = Configuration::upgrade(key(), previous.clone());
    let wire = serde_json::to_value(configuration).unwrap();
    assert_eq!(wire["version"], 4);
    assert_eq!(wire["storage"], "upgrade");
    let recovered: Configuration = serde_json::from_value(wire).unwrap();
    assert_eq!(recovered.upgrade_from(), Some(&previous));
    assert_eq!(recovered.admin_key(), &key());
    assert_eq!(recovered.storage(), Storage::Upgrade);
}

#[test]
fn upgrade_authority_is_not_accepted_by_ordinary_resume_or_allocation() {
    for (version, storage) in [
        (1, "allocate"),
        (2, "preallocated"),
        (3, "resume"),
        (4, "resume"),
    ] {
        assert!(
            serde_json::from_value::<Configuration>(json!({
                "version":version,"admin_key":key(),"storage":storage,
                "upgrade_from":"/nix/store/00000000000000000000000000000000-bootstrap"
            }))
            .is_err()
        );
    }
    assert!(
        serde_json::from_value::<Configuration>(
            json!({"version":4,"admin_key":key(),"storage":"upgrade"})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<Configuration>(json!({"version":4,"admin_key":key(),"storage":"upgrade","upgrade_from":"/host/database"})).is_err());
}

#[test]
fn ordinary_resume_does_not_replay_prior_upgrade_authority() {
    let prior: StorePath = "/nix/store/00000000000000000000000000000000-bootstrap"
        .to_owned()
        .try_into()
        .unwrap();
    let upgraded = Configuration::upgrade(key(), prior);
    let resumed = Configuration::resume(upgraded.admin_key().clone());
    let wire = serde_json::to_value(resumed).unwrap();
    assert_eq!(wire["version"], 3);
    assert_eq!(wire["storage"], "resume");
    assert!(wire.get("upgrade_from").is_none());
}

#[test]
fn storage_versions_unknown_fields_and_paths_cannot_change_disk_policy() {
    for (version, storage) in [
        (0, None),
        (2, None),
        (3, Some("preallocated")),
        (1, Some("preallocated")),
        (2, Some("allocate")),
        (2, Some("/host/disk")),
    ] {
        let mut wire = json!({"version": version, "admin_key": key()});
        if let Some(storage) = storage {
            wire["storage"] = json!(storage);
        }
        assert!(serde_json::from_value::<Configuration>(wire).is_err());
    }
    let mut wire = json!({"version": 2, "admin_key": key(), "storage": "preallocated"});
    wire["path"] = json!("/host/disk");
    assert!(serde_json::from_value::<Configuration>(wire).is_err());
}
