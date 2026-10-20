use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

use super::*;

#[test]
fn private_namespace_encodes_the_complete_workspace_hash_within_native_limits() {
    for byte in 0u8..=255 {
        let hash = format!("{byte:02x}").repeat(32);
        let workspace = WorkspaceHash::parse(&hash).unwrap();
        let namespace = private_namespace(&workspace).unwrap();
        assert!(namespace.as_str().len() <= attic::cache::MAX_NAME_LENGTH);
        let decoded = URL_SAFE_NO_PAD
            .decode(namespace.as_str().strip_prefix("wrix-").unwrap())
            .unwrap();
        assert_eq!(decoded, vec![byte; 32]);
    }
}

#[tokio::test]
async fn namespace_lock_timeout_does_not_replace_the_existing_lock() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cache.lock");
    fs::write(&path, "").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let held = acquire(root.path(), Duration::ZERO).await.unwrap();
    let inode = held.metadata().unwrap().ino();
    assert!(matches!(
        acquire(root.path(), Duration::ZERO).await,
        Err(Error::LockTimeout)
    ));
    drop(held);
    let acquired = acquire(root.path(), Duration::from_secs(1)).await.unwrap();
    assert_eq!(acquired.metadata().unwrap().ino(), inode);
}

#[tokio::test]
async fn namespace_lock_refuses_symlink_and_hardlink_targets() {
    let root = tempfile::tempdir().unwrap();
    let other = root.path().join("other");
    fs::write(&other, "preserve").unwrap();
    let path = root.path().join("cache.lock");
    symlink(&other, &path).unwrap();
    assert!(acquire(root.path(), Duration::ZERO).await.is_err());
    fs::remove_file(&path).unwrap();
    fs::hard_link(&other, &path).unwrap();
    assert!(matches!(
        acquire(root.path(), Duration::ZERO).await,
        Err(Error::UnsafeRegistrationFile)
    ));
    assert_eq!(fs::read_to_string(&other).unwrap(), "preserve");
}

#[test]
fn public_key_recording_replaces_readable_keys_but_not_unsafe_targets() {
    let root = tempfile::tempdir().unwrap();
    let keys = root.path().join("keys");
    fs::create_dir(&keys).unwrap();
    let path = keys.join("cache.pub");
    fs::write(&path, "old public key").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let key =
        CachePublicKey::parse("fixture:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").unwrap();
    record_key(root.path(), &key).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        format!("{}\n", key.as_str())
    );
    let other = root.path().join("other");
    fs::write(&other, "preserve").unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&other, &path).unwrap();
    assert!(record_key(root.path(), &key).is_err());
    assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
    fs::remove_file(&path).unwrap();
    fs::hard_link(&other, &path).unwrap();
    assert!(matches!(
        record_key(root.path(), &key),
        Err(Error::UnsafeRegistrationFile)
    ));
    assert_eq!(fs::read_to_string(&other).unwrap(), "preserve");
}
