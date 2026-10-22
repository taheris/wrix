use super::*;
use crate::control::Access;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn legacy(root: &Path) -> TestResult<Vec<u8>> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "version":1, "entries":[{"namespace":"a".repeat(64), "policy": {
            "dolt":"denied", "private_cache_read":"allowed", "common_cache_read":"allowed", "egress":"denied", "compiler":"denied"
        }}]
    }))?;
    fs::write(root.join("namespaces.json"), &bytes)?;
    fs::set_permissions(
        root.join("namespaces.json"),
        fs::Permissions::from_mode(0o600),
    )?;
    Ok(bytes)
}

#[test]
fn cached_legacy_controller_must_be_excluded_before_registry_epoch_migration() -> TestResult {
    let root = tempfile::tempdir()?;
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
    let bytes = legacy(root.path())?;
    // A v1 controller retains this original kernel lock while issuing from its
    // cached map. It knows nothing about new metadata/namespace lease files.
    let old_controller = Ownership::acquire(root.path())?;
    let store = Store::open(root.path())?;
    assert!(matches!(store.lock(), Err(Error::RegistryUpgrade)));
    assert!(Ownership::acquire(root.path()).is_err());
    assert_eq!(fs::read(root.path().join("namespaces.json"))?, bytes);
    // Even another cached-v1 commit cannot make the new host writer admit SQL.
    legacy(root.path())?;
    assert!(matches!(store.lock(), Err(Error::RegistryUpgrade)));
    assert_eq!(fs::read(root.path().join("namespaces.json"))?, bytes);
    drop(old_controller);
    // Stopping alone is not implicit migration; explicit new-controller startup
    // or service recovery must acquire the original ownership proof.
    assert!(matches!(store.lock(), Err(Error::RegistryUpgrade)));
    let maintenance = Ownership::acquire(root.path())?;
    store.initialize(&maintenance)?;
    let registry = store.lock()?;
    let namespace = Namespace::try_from("a".repeat(64))?;
    assert_eq!(registry[&namespace].dolt, Access::Denied);
    assert_eq!(registry[&namespace].private_cache_read, Access::Allowed);
    let migrated: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("namespaces.json"))?)?;
    assert_eq!(migrated["version"], 2);
    // Native v1's load rejects every snapshot whose version is not 1: an old
    // binary restart fails closed instead of rejoining with cached semantics.
    assert_ne!(migrated["version"], 1);
    Ok(())
}

#[test]
fn old_controller_with_no_snapshot_cannot_be_mistaken_for_fresh_authority() -> TestResult {
    let root = tempfile::tempdir()?;
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
    let old_controller = Ownership::acquire(root.path())?;
    let store = Store::open(root.path())?;
    assert!(matches!(store.lock(), Err(Error::RegistryUpgrade)));
    assert!(!root.path().join("namespaces.json").exists());
    drop(old_controller);
    assert!(store.lock()?.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(
            root.path().join("namespaces.json")
        )?)?["version"],
        2
    );
    Ok(())
}
