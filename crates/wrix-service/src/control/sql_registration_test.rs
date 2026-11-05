use super::*;

#[tokio::test]
async fn sql_publication_requires_durable_ready_registration_and_preserves_peer_policy() {
    let state = tempfile::tempdir().unwrap();
    let root = state.path().join("control/authority");
    wrix_core::private_directory::prepare_with_parents(&root).unwrap();
    let namespace = Namespace::from(&WorkspaceHash::parse(&"d".repeat(64)).unwrap());
    let controller = Controller::bind(&root).unwrap();
    let store = registration::Store::open(&root).unwrap();
    let lease = store.lease(&namespace).unwrap();
    let policy = Policy {
        private_cache_read: Access::Allowed,
        compiler: Access::Allowed,
        ..denied_policy()
    };
    {
        let mut registry = store.lock().unwrap();
        registry.insert(namespace.clone(), policy);
        registry.commit().unwrap();
        registry.prepare_sql(&namespace).unwrap();
    }
    let native = state
        .path()
        .join("host-control/dolt")
        .join(namespace.as_str());
    wrix_core::private_directory::prepare_with_parents(&native).unwrap();
    let path = native.join("registration.json");
    // A metadata fixture, not proof that a native SQL server is running.
    let mut record = serde_json::json!({
        "version":1, "namespace":namespace.as_str(), "branch":"beads",
        "phase":"restoring", "credentials":{
            "database":format!("n{}", "d".repeat(63)),
            "user":"d".repeat(32), "password":"a".repeat(64)
        }
    });
    for phase in ["restoring", "imported"] {
        record["phase"] = phase.into();
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(commit_sql_registration(state.path(), &namespace, &lease).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(store.lock().unwrap().get(&namespace), Some(&policy));
    }
    record["phase"] = "ready".into();
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    commit_sql_registration(state.path(), &namespace, &lease).unwrap();
    assert_eq!(required_sql(state.path()).unwrap(), vec![namespace.clone()]);
    assert_eq!(
        store.lock().unwrap().get(&namespace),
        Some(&Policy {
            dolt: Access::Allowed,
            ..policy
        })
    );
    controller.close().unwrap();
}
