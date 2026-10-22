use wrix_core::nix_store::pool::{Key, Repository, System};

#[test]
fn pool_key_is_repository_and_guest_system_not_a_transport_or_permission() {
    let repository = Repository::try_from("a".repeat(32)).unwrap();
    let x86 = Key {
        repository: repository.clone(),
        system: System::X86_64Linux,
    };
    let arm = Key {
        repository,
        system: System::Aarch64Linux,
    };
    assert_ne!(x86, arm);
    assert_eq!(
        serde_json::to_value(&x86).unwrap()["system"],
        "x86_64-linux"
    );
    assert_eq!(
        serde_json::to_value(&arm).unwrap()["system"],
        "aarch64-linux"
    );
    assert_eq!(
        serde_json::from_value::<Key>(serde_json::to_value(&x86).unwrap()).unwrap(),
        x86
    );
}

#[test]
fn pool_identity_rejects_aliases_host_systems_and_authority_fields() {
    for value in [
        String::new(),
        "a".repeat(31),
        "a".repeat(33),
        "A".repeat(32),
        "../peer".into(),
    ] {
        assert!(Repository::try_from(value).is_err());
    }
    for system in [
        "x86_64-darwin",
        "aarch64-darwin",
        "x86_64-linux ",
        "host",
        "auto",
    ] {
        assert!(
            serde_json::from_value::<Key>(
                serde_json::json!({"repository":"a".repeat(32),"system":system})
            )
            .is_err()
        );
    }
    for field in ["path", "endpoint", "approved", "namespace"] {
        let mut value = serde_json::json!({"repository":"a".repeat(32),"system":"x86_64-linux"});
        value[field] = serde_json::json!("caller-chosen");
        assert!(serde_json::from_value::<Key>(value).is_err());
    }
}
