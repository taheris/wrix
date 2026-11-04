use super::fixture::{Fixture, snapshot};
use std::{fs, path::Path, process::Output};
use wrix_core::path::WorkspaceHash;

pub fn retain(fixture: &Fixture, checkout: &Path) -> WorkspaceHash {
    let key = WorkspaceHash::from_path(checkout);
    let root = fixture.state().join("workspaces").join(key.as_str());
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("services.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "workspace_hash": key.as_str(),
            "workspace_path": checkout,
            "container_name": "retained-service",
            "endpoints": {}
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(root.join("retained-history"), b"never discard former data").unwrap();
    let imported = fixture.run(&["import-native", "--native-key", key.as_str()]);
    assert!(imported.status.success(), "{:?}", imported);
    key
}

pub fn associate(
    fixture: &Fixture,
    primary: &Path,
    checkout: &Path,
    key: &WorkspaceHash,
) -> Output {
    fixture.run(&[
        "associate",
        "--primary",
        primary.to_str().unwrap(),
        "--checkout",
        checkout.to_str().unwrap(),
        "--retain",
        key.as_str(),
        "--preserve-former",
    ])
}

pub fn inspect(fixture: &Fixture, checkout: &Path) -> Output {
    fixture.run(&["inspect", "--checkout", checkout.to_str().unwrap()])
}

#[test]
fn explicit_host_association_shares_the_existing_namespace_without_copying_payloads() {
    let fixture = Fixture::new();
    fixture.initialize();
    let primary = fixture.repository("primary");
    let checkout = fixture.repository("checkout");
    let unrelated = fixture.repository("unrelated");
    let key = retain(&fixture, &primary);
    let former = retain(&fixture, &checkout);
    let payloads = snapshot(&fixture.state().join("workspaces"));
    let output = associate(&fixture, &primary, &checkout, &key);
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(snapshot(&fixture.state().join("workspaces")), payloads);
    for caller in [&primary, &checkout] {
        let result = inspect(&fixture, caller);
        assert!(result.status.success(), "{:?}", result);
        let text = String::from_utf8(result.stdout).unwrap();
        assert!(text.contains(&format!("native_key: {key}")));
        assert!(text.contains(&format!("scope: {}", primary.display())));
        assert!(text.contains(&format!("caller: {}", caller.display())));
        assert!(text.contains("integrated: true"));
        let endpoints = fixture.application(caller, &["service", "endpoints"]);
        assert!(endpoints.status.success(), "{:?}", endpoints);
        let value: serde_json::Value = serde_json::from_slice(&endpoints.stdout).unwrap();
        assert_eq!(value["workspace_hash"], key.as_str());
    }
    let output = inspect(&fixture, &unrelated);
    assert!(output.status.success(), "{:?}", output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("integrated: false"));
    assert!(!text.contains(key.as_str()));
    let connection = rusqlite::Connection::open(fixture.state().join("state.db")).unwrap();
    let observed: String = connection
        .query_row(
            "SELECT slot FROM binding WHERE native_key = ?1",
            [key.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(observed, "wrix-host-association");
    let receipt: Vec<u8> = connection
        .query_row(
            "SELECT value FROM namespace_metadata WHERE native_key = ?1 AND owner = 'namespace/associated-former'",
            [former.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
    assert_eq!(receipt["destination"], key.as_str());
}

#[test]
fn repeated_host_association_is_byte_idempotent() {
    let fixture = Fixture::new();
    fixture.initialize();
    let primary = fixture.repository("primary");
    let checkout = fixture.repository("checkout");
    let key = retain(&fixture, &primary);
    assert!(
        associate(&fixture, &primary, &checkout, &key)
            .status
            .success()
    );
    let before = snapshot(&fixture.state());
    assert!(
        associate(&fixture, &primary, &checkout, &key)
            .status
            .success()
    );
    assert_eq!(snapshot(&fixture.state()), before);
    assert!(inspect(&fixture, &checkout).status.success());
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn host_association_refuses_reassignment_to_another_primary() {
    let fixture = Fixture::new();
    fixture.initialize();
    let primary = fixture.repository("primary");
    let other = fixture.repository("other");
    let checkout = fixture.repository("checkout");
    let key = retain(&fixture, &primary);
    let other_key = retain(&fixture, &other);
    assert!(
        associate(&fixture, &primary, &checkout, &key)
            .status
            .success()
    );
    let before = snapshot(&fixture.state());
    assert!(
        !associate(&fixture, &other, &checkout, &other_key)
            .status
            .success()
    );
    assert_eq!(snapshot(&fixture.state()), before);
    let output = inspect(&fixture, &checkout);
    assert!(output.status.success(), "{:?}", output);
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains(key.as_str())
    );
}

#[test]
fn host_association_requires_retention_and_explicit_former_state_consent() {
    let fixture = Fixture::new();
    fixture.initialize();
    let primary = fixture.repository("primary");
    let checkout = fixture.repository("checkout");
    let key = retain(&fixture, &primary);
    let before = snapshot(&fixture.state());
    let no_consent = fixture.run(&[
        "associate",
        "--primary",
        primary.to_str().unwrap(),
        "--checkout",
        checkout.to_str().unwrap(),
        "--retain",
        key.as_str(),
    ]);
    assert!(!no_consent.status.success());
    let wrong = WorkspaceHash::parse(&"a".repeat(64)).unwrap();
    assert!(
        !associate(&fixture, &primary, &checkout, &wrong)
            .status
            .success()
    );
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn replaced_associated_git_administration_cannot_inherit_or_reconfirm_access() {
    let fixture = Fixture::new();
    fixture.initialize();
    let primary = fixture.repository("primary");
    let checkout = fixture.repository("checkout");
    let key = retain(&fixture, &primary);
    assert!(
        associate(&fixture, &primary, &checkout, &key)
            .status
            .success()
    );
    let actual = fixture.root.path().join("checkout");
    fs::rename(actual.join(".git"), actual.join("retained-git")).unwrap();
    let replacement = fixture.git(&checkout, &["init", "-q"]);
    assert!(replacement.status.success(), "{:?}", replacement);
    let before = snapshot(&fixture.state());
    assert!(!inspect(&fixture, &checkout).status.success());
    assert!(
        !associate(&fixture, &primary, &checkout, &key)
            .status
            .success()
    );
    assert_eq!(snapshot(&fixture.state()), before);
    assert!(inspect(&fixture, &primary).status.success());
}

#[test]
fn replaced_primary_git_administration_invalidates_all_associated_access() {
    let fixture = Fixture::new();
    fixture.initialize();
    let primary = fixture.repository("primary");
    let checkout = fixture.repository("checkout");
    let key = retain(&fixture, &primary);
    assert!(
        associate(&fixture, &primary, &checkout, &key)
            .status
            .success()
    );
    let actual = fixture.root.path().join("primary");
    fs::rename(actual.join(".git"), actual.join("retained-git")).unwrap();
    assert!(fixture.git(&primary, &["init", "-q"]).status.success());
    let before = snapshot(&fixture.state());
    assert!(!inspect(&fixture, &checkout).status.success());
    assert!(!inspect(&fixture, &primary).status.success());
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn reserved_association_slots_cannot_select_an_external_executable() {
    let fixture = Fixture::new();
    fixture.initialize();
    let before = snapshot(&fixture.state());
    let output = fixture.run(&[
        "integration",
        "select",
        "--slot",
        "wrix-host-association",
        "--executable",
        "/not-used",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reserved host association slot"));
    assert_eq!(snapshot(&fixture.state()), before);
}
