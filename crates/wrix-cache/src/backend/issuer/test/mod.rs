use std::os::unix::fs::{PermissionsExt, symlink};

use super::*;

fn directories() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("host-authority");
    let metadata = temporary.path().join("delegated-metadata");
    fs::create_dir(&metadata).unwrap();
    (temporary, root, metadata)
}

#[test]
fn provisioning_preserves_issuer_and_delegates_only_its_public_key() {
    let (_temporary, root, metadata) = directories();
    Authority::provision(&root, &metadata).unwrap();
    let private = fs::read(root.join(PRIVATE_NAME)).unwrap();
    let public = fs::read(metadata.join("jwt.pub")).unwrap();
    Authority::provision(&root, &metadata).unwrap();
    assert_eq!(fs::read(root.join(PRIVATE_NAME)).unwrap(), private);
    assert_eq!(fs::read(metadata.join("jwt.pub")).unwrap(), public);
    assert!(public.starts_with(b"-----BEGIN PUBLIC KEY-----"));
    assert_eq!(fs::read_dir(&metadata).unwrap().count(), 1);
    assert_eq!(
        fs::metadata(root.join(PRIVATE_NAME)).unwrap().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
}

#[test]
fn exact_view_publication_tokens_verify_with_delegated_public_authority() {
    let (_temporary, root, metadata) = directories();
    let authority = Authority::provision(&root, &metadata).unwrap();
    for namespace in ["private-a", "private-b", "common"] {
        let token = authority
            .publication_token(&CacheName::new(namespace.into()).unwrap())
            .unwrap();
        let parts: Vec<_> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header, serde_json::json!({"alg":"RS256", "typ":"JWT"}));
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        let grants = claims["https://jwt.attic.rs/v1"]["caches"]
            .as_object()
            .unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(
            grants[namespace],
            serde_json::json!({"r":1,"w":1,"cc":1,"cr":1,"cq":1,"cd":1})
        );
        assert_eq!(
            claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(),
            PUBLICATION_LIFETIME_SECONDS
        );
        let signature = root.join("signature");
        let message = root.join("message");
        fs::write(&signature, URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        fs::write(&message, format!("{}.{}", parts[0], parts[1])).unwrap();
        let verified = Command::new("openssl")
            .args(["dgst", "-sha256", "-verify"])
            .arg(metadata.join("jwt.pub"))
            .arg("-signature")
            .arg(&signature)
            .arg(&message)
            .output()
            .unwrap();
        assert!(verified.status.success(), "{:?}", verified.stderr);
        fs::write(&message, "tampered").unwrap();
        let rejected = Command::new("openssl")
            .args(["dgst", "-sha256", "-verify"])
            .arg(metadata.join("jwt.pub"))
            .arg("-signature")
            .arg(&signature)
            .arg(&message)
            .output()
            .unwrap();
        assert!(!rejected.status.success());
    }
    assert!(CacheName::new("private-*".into()).is_err());
}

#[test]
fn mismatching_verifier_preserves_existing_issuer_and_verifier() {
    let (_temporary, root, metadata) = directories();
    Authority::provision(&root, &metadata).unwrap();
    let private = fs::read(root.join(PRIVATE_NAME)).unwrap();
    fs::write(metadata.join("jwt.pub"), "different authority").unwrap();
    assert!(matches!(
        Authority::provision(&root, &metadata),
        Err(Error::VerifierMismatch)
    ));
    assert_eq!(fs::read(root.join(PRIVATE_NAME)).unwrap(), private);
    assert_eq!(
        fs::read_to_string(metadata.join("jwt.pub")).unwrap(),
        "different authority"
    );
}

#[test]
fn invalid_existing_issuer_is_not_replaced_or_delegated() {
    let (_temporary, root, metadata) = directories();
    wrix_core::private_directory::prepare(&root).unwrap();
    fs::write(root.join(PRIVATE_NAME), "invalid authority").unwrap();
    fs::set_permissions(root.join(PRIVATE_NAME), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        Authority::provision(&root, &metadata),
        Err(Error::Crypto)
    ));
    assert_eq!(
        fs::read_to_string(root.join(PRIVATE_NAME)).unwrap(),
        "invalid authority"
    );
    assert_eq!(fs::read_dir(metadata).unwrap().count(), 0);
}

#[test]
fn shared_or_linked_issuer_files_fail_without_changing_targets() {
    let (temporary, root, metadata) = directories();
    wrix_core::private_directory::prepare(&root).unwrap();
    let target = temporary.path().join("unrelated");
    fs::write(&target, "preserved").unwrap();
    symlink(&target, root.join(PRIVATE_NAME)).unwrap();
    assert!(Authority::provision(&root, &metadata).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "preserved");
    fs::remove_file(root.join(PRIVATE_NAME)).unwrap();
    fs::write(root.join(PRIVATE_NAME), "preserved").unwrap();
    fs::set_permissions(root.join(PRIVATE_NAME), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        Authority::provision(&root, &metadata),
        Err(Error::Permissions)
    ));
    assert_eq!(
        fs::read_to_string(root.join(PRIVATE_NAME)).unwrap(),
        "preserved"
    );
    assert_eq!(fs::read_dir(metadata).unwrap().count(), 0);
}

#[test]
fn authority_cannot_be_provisioned_inside_delegated_metadata() {
    let (_temporary, _root, metadata) = directories();
    let nested = metadata.join("issuer");
    assert!(matches!(
        Authority::provision(&nested, &metadata),
        Err(Error::Layout)
    ));
    assert!(!nested.join(PRIVATE_NAME).exists());
    assert!(!metadata.join("jwt.pub").exists());
}

#[test]
fn missing_verifier_is_restored_from_the_unchanged_issuer() {
    let (_temporary, root, metadata) = directories();
    Authority::provision(&root, &metadata).unwrap();
    let private = fs::read(root.join(PRIVATE_NAME)).unwrap();
    let public = fs::read(metadata.join("jwt.pub")).unwrap();
    fs::remove_file(metadata.join("jwt.pub")).unwrap();
    Authority::provision(&root, &metadata).unwrap();
    assert_eq!(fs::read(root.join(PRIVATE_NAME)).unwrap(), private);
    assert_eq!(fs::read(metadata.join("jwt.pub")).unwrap(), public);
}

#[test]
fn initial_verifier_conflict_does_not_commit_new_issuer_authority() {
    let (_temporary, root, metadata) = directories();
    fs::write(metadata.join("jwt.pub"), "preexisting authority").unwrap();
    assert!(matches!(
        Authority::provision(&root, &metadata),
        Err(Error::VerifierMismatch)
    ));
    assert!(!root.join(PRIVATE_NAME).exists());
    assert_eq!(
        fs::read_to_string(metadata.join("jwt.pub")).unwrap(),
        "preexisting authority"
    );
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}

#[test]
fn concurrent_provisioning_preserves_one_issuer() {
    let (_temporary, root, metadata) = directories();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| Authority::provision(&root, &metadata).unwrap()))
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let private = fs::read(root.join(PRIVATE_NAME)).unwrap();
    let public = fs::read(metadata.join("jwt.pub")).unwrap();
    assert_eq!(public_key(&root.join(PRIVATE_NAME), &root).unwrap(), public);
    Authority::provision(&root, &metadata).unwrap();
    assert_eq!(fs::read(root.join(PRIVATE_NAME)).unwrap(), private);
}
