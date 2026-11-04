use super::*;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires a native Apple container image inventory"]
fn native_inventory_index_and_config_inspection_agree() {
    let images = run_required_output("container", &["image", "list", "--format", "json"]).unwrap();
    let rows = super::super::container_image_rows(&images.stdout).unwrap();
    assert!(!rows.is_empty(), "native inventory must contain an image");
    for row in rows {
        let inspected =
            run_required_output("container", &["image", "inspect", row.target.as_str()]).unwrap();
        let value: Value = serde_json::from_slice(&inspected.stdout).unwrap();
        assert!(
            index(&value).is_ok(),
            "native index for {}",
            row.target.as_str()
        );
        let config = super::super::container_content_digest(&value);
        assert!(
            config.is_ok(),
            "native config for {}: {config:?}",
            row.target.as_str()
        );
    }
}

#[test]
fn native_index_digest_cannot_impersonate_an_oci_config_digest() {
    let index = "a".repeat(64);
    let value =
        json!([{"id":index,"configuration":{"descriptor":{"digest":format!("sha256:{index}")}}}]);
    assert_eq!(
        super::index(&value).unwrap().as_str(),
        format!("sha256:{index}")
    );
    let mut wrong = value;
    wrong[0]["id"] = json!("b".repeat(64));
    assert!(super::index(&wrong).is_err());
}
#[test]
fn attestation_hashes_exact_configuration_bytes_not_a_platform_index() {
    let config =
        br#"{"architecture":"arm64","os":"linux","rootfs":{"type":"layers","diff_ids":[]}}"#;
    let digest = config_digest(config).unwrap();
    assert_eq!(
        digest.as_str(),
        "sha256:04c343465ae76ae68bc20cba183c3cebbc6f2cee9a5009e83ebd1667a707f283"
    );
    assert_ne!(
        digest,
        config_digest(&[config.as_slice(), b"\n"].concat()).unwrap()
    );
    assert!(config_digest(br#"{"schemaVersion":2,"manifests":[]}"#).is_err());
    assert!(config_digest(b"null").is_err());
}

#[test]
fn passive_missing_mapping_does_not_create_any_path() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("absent");
    assert_eq!(
        read(
            &root,
            &Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap()
        )
        .unwrap(),
        None
    );
    assert!(!root.exists());
}
#[test]
fn config_receipt_is_bound_to_exact_native_index_and_private_metadata() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let index = Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap();
    let config = Digest::parse(&format!("sha256:{}", "b".repeat(64))).unwrap();
    let path = root.path().join("a".repeat(64));
    fs::write(
        &path,
        serde_json::to_vec(&Receipt {
            version: 2,
            index: index.clone(),
            config: config.clone(),
            system: system().unwrap(),
        })
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(read(root.path(), &index).unwrap(), Some(config));
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read(root.path(), &index).is_err());
}

#[test]
fn a_receipt_for_another_native_index_is_not_a_cache_miss() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let expected = Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap();
    let foreign = Digest::parse(&format!("sha256:{}", "b".repeat(64))).unwrap();
    let path = root.path().join("a".repeat(64));
    fs::write(
        &path,
        serde_json::to_vec(&Receipt {
            version: 2,
            index: foreign.clone(),
            config: foreign,
            system: system().unwrap(),
        })
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        read(root.path(), &expected),
        Err(Error::InvalidMetadata)
    ));
}

#[test]
fn native_inspection_must_match_requested_id_and_its_descriptor() {
    let id = "a".repeat(64);
    let expected = ImageId::parse(&format!("sha256:{id}")).unwrap();
    let value = json!([{"id":id,"configuration":{"descriptor":{"digest":format!("sha256:{id}")}}}]);
    require_id(&serde_json::to_vec(&value).unwrap(), &expected).unwrap();
    let wrong = ImageId::parse(&"b".repeat(64)).unwrap();
    assert!(require_id(&serde_json::to_vec(&value).unwrap(), &wrong).is_err());
    let mut malformed = value;
    malformed[0]["configuration"]["descriptor"]["digest"] =
        json!(format!("sha256:{}", "c".repeat(64)));
    assert!(require_id(&serde_json::to_vec(&malformed).unwrap(), &expected).is_err());
}

#[test]
fn a_symlink_receipt_is_not_followed() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let target = root.path().join("target");
    fs::write(&target, b"not an attestation").unwrap();
    std::os::unix::fs::symlink(&target, root.path().join("a".repeat(64))).unwrap();
    let expected = Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap();
    assert!(read(root.path(), &expected).is_err());
    assert_eq!(fs::read(target).unwrap(), b"not an attestation");
}
