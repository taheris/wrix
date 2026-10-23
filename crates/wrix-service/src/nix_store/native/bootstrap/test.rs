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
