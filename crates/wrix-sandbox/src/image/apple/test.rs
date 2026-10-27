use super::*;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
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
            version: 1,
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
            version: 1,
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
