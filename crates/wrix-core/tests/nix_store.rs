use serde_json::json;
use wrix_core::nix_store::{Image, SeedClosure, Version};

const SEED: &str = "/nix/store/00000000000000000000000000000000-closure-info/store-paths";

#[test]
fn shared_image_contract_round_trips_without_granting_admission() {
    let wire = json!({"version": 1, "seed_closure": SEED});
    let image: Image = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(image.version, Version::CURRENT);
    assert_eq!(image.seed_closure.as_path().to_str(), Some(SEED));
    assert_eq!(serde_json::to_value(image).unwrap(), wire);
}

#[test]
fn future_versions_are_readable_but_never_compatible() {
    let image: Image = serde_json::from_value(json!({"version": 2, "seed_closure": SEED})).unwrap();
    assert!(image.version.require_supported().is_err());
    assert!(Version::CURRENT.require_supported().is_ok());
    assert!(
        image
            .version
            .require_supported()
            .unwrap_err()
            .to_string()
            .contains("v2")
    );
}

#[test]
fn shared_image_contract_requires_typed_complete_metadata() {
    for wire in [
        json!({}),
        json!({"version": 1}),
        json!({"seed_closure": SEED}),
        json!({"version": 0, "seed_closure": SEED}),
        json!({"version": -1, "seed_closure": SEED}),
        json!({"version": "1", "seed_closure": SEED}),
        json!({"version": true, "seed_closure": SEED}),
        json!({"version": 1, "seed_closure": SEED, "trusted": true}),
    ] {
        assert!(
            serde_json::from_value::<Image>(wire.clone()).is_err(),
            "{wire}"
        );
    }
}

#[test]
fn seed_metadata_cannot_name_mutable_or_noncanonical_host_paths() {
    for path in [
        "/tmp/store-paths",
        "store-paths",
        "/nix/store/store-paths",
        "/nix/store/00000000000000000000000000000000-closure-info/registration",
        "/nix/store/00000000000000000000000000000000-closure-info/../store-paths",
        "/nix/store/00000000000000000000000000000000-closure-info//store-paths",
        "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-closure-info/store-paths",
        "/nix/store/0000000000000000000000000000000-closure-info/store-paths",
        "/nix/store/00000000000000000000000000000000-/store-paths",
        "/nix/store/00000000000000000000000000000000-bad name/store-paths",
    ] {
        assert!(SeedClosure::parse(path).is_err(), "{path}");
    }
    assert!(SeedClosure::parse(SEED).is_ok());
}
