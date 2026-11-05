use super::*;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn unregistered_preparation_never_publishes_sql_or_poison_cold_readiness() {
    let state = tempfile::tempdir().unwrap();
    let root = state.path().join("control/authority");
    wrix_core::private_directory::prepare_with_parents(&root).unwrap();
    let namespace = Namespace::from(&WorkspaceHash::parse(&"a".repeat(64)).unwrap());
    let controller = Controller::bind(&root).unwrap();
    let store = registration::Store::open(&root).unwrap();
    let lease = store.lease(&namespace).unwrap();
    store.lock().unwrap().prepare_sql(&namespace).unwrap();
    let native = state
        .path()
        .join("host-control/dolt")
        .join(namespace.as_str());
    fs::create_dir_all(&native).unwrap();
    fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(commit_sql_registration(state.path(), &namespace, &lease).is_err());
    assert!(!native.join("registration.json").exists());
    assert_eq!(required_sql(state.path()).unwrap(), Vec::<Namespace>::new());
    drop(lease);
    controller.close().unwrap();
    let restarted = Controller::bind(&root).unwrap();
    assert!(matches!(
        restarted.authorize(&namespace, Permission::Sql),
        Err(Error::PermissionDenied)
    ));
    assert_eq!(
        store.lock().unwrap().get(&namespace),
        Some(&denied_policy())
    );
    restarted.close().unwrap();
}

#[tokio::test]
async fn failed_repreparation_preserves_an_existing_sql_policy_and_other_audiences() {
    let state = tempfile::tempdir().unwrap();
    let root = state.path().join("control/authority");
    wrix_core::private_directory::prepare_with_parents(&root).unwrap();
    let namespace = Namespace::from(&WorkspaceHash::parse(&"b".repeat(64)).unwrap());
    let controller = Controller::bind(&root).unwrap();
    let store = registration::Store::open(&root).unwrap();
    let policy = Policy {
        dolt: Access::Allowed,
        private_cache_read: Access::Allowed,
        compiler: Access::Allowed,
        ..denied_policy()
    };
    {
        let mut registry = store.lock().unwrap();
        registry.insert(namespace.clone(), policy);
        registry.commit().unwrap();
    }
    let before = fs::read(root.join("namespaces.json")).unwrap();
    let lease = store.lease(&namespace).unwrap();
    store.lock().unwrap().prepare_sql(&namespace).unwrap();
    assert!(commit_sql_registration(state.path(), &namespace, &lease).is_err());
    assert_eq!(fs::read(root.join("namespaces.json")).unwrap(), before);
    assert_eq!(required_sql(state.path()).unwrap(), vec![namespace.clone()]);
    assert_eq!(store.lock().unwrap().get(&namespace), Some(&policy));
    controller.close().unwrap();
}

#[tokio::test]
async fn preparing_sql_preserves_existing_denied_sql_and_cache_permissions() {
    let state = tempfile::tempdir().unwrap();
    let root = state.path().join("control/authority");
    wrix_core::private_directory::prepare_with_parents(&root).unwrap();
    let namespace = Namespace::from(&WorkspaceHash::parse(&"c".repeat(64)).unwrap());
    let controller = Controller::bind(&root).unwrap();
    let store = registration::Store::open(&root).unwrap();
    let policy = Policy {
        private_cache_read: Access::Allowed,
        common_cache_read: Access::Allowed,
        ..denied_policy()
    };
    {
        let mut registry = store.lock().unwrap();
        registry.insert(namespace.clone(), policy);
        registry.commit().unwrap();
    }
    let before = fs::read(root.join("namespaces.json")).unwrap();
    store.lock().unwrap().prepare_sql(&namespace).unwrap();
    assert_eq!(fs::read(root.join("namespaces.json")).unwrap(), before);
    assert_eq!(required_sql(state.path()).unwrap(), Vec::<Namespace>::new());
    assert_eq!(store.lock().unwrap().get(&namespace), Some(&policy));
    controller.close().unwrap();
}
