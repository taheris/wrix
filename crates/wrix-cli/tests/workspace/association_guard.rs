use super::{association, fixture::Fixture};
use std::{path::Path, process::Output};

#[test]
fn concurrent_matching_host_associations_preserve_one_binding() {
    let fixture = Fixture::new();
    fixture.initialize();
    let primary = fixture.repository("primary");
    let checkout = fixture.repository("checkout");
    let key = association::retain(&fixture, &primary);
    let outputs: Vec<Output> = std::thread::scope(|scope| {
        let handles: [_; 4] = std::array::from_fn(|_| {
            scope.spawn(|| association::associate(&fixture, &primary, &checkout, &key))
        });
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });
    assert!(
        outputs.iter().all(|output| output.status.success()),
        "{outputs:?}"
    );
    let connection = rusqlite::Connection::open(fixture.state().join("state.db")).unwrap();
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM binding", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert!(association::inspect(&fixture, &checkout).status.success());
}

#[test]
fn malformed_former_namespace_receipts_refuse_without_repair() {
    let fixture = Fixture::new();
    fixture.initialize();
    let primary = fixture.repository("primary");
    let checkout = fixture.repository("checkout");
    let key = association::retain(&fixture, &primary);
    let former = association::retain(&fixture, &checkout);
    assert!(
        association::associate(&fixture, &primary, &checkout, &key)
            .status
            .success()
    );
    let connection = rusqlite::Connection::open(fixture.state().join("state.db")).unwrap();
    let bytes: Vec<u8> = connection.query_row(
        "SELECT value FROM namespace_metadata WHERE native_key = ?1 AND owner = 'namespace/associated-former'",
        [former.as_str()], |row| row.get(0),
    ).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for (field, value) in [
        ("version", serde_json::json!(2)),
        ("destination", serde_json::json!(former.as_str())),
        ("evidence", serde_json::json!([])),
        ("checkout", serde_json::json!([0])),
        ("unknown", serde_json::json!(true)),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        connection.execute(
            "UPDATE namespace_metadata SET value = ?1 WHERE native_key = ?2 AND owner = 'namespace/associated-former'",
            rusqlite::params![serde_json::to_vec(&changed).unwrap(), former.as_str()],
        ).unwrap();
        let before = super::fixture::snapshot(&fixture.state());
        assert!(
            !association::inspect(&fixture, &checkout).status.success(),
            "accepted {field}"
        );
        assert_eq!(super::fixture::snapshot(&fixture.state()), before);
    }
}

#[test]
fn a_captured_standalone_context_cannot_survive_explicit_association() {
    const PRIMARY: &str = "WRIX_TEST_ASSOCIATION_PRIMARY";
    const CHECKOUT: &str = "WRIX_TEST_ASSOCIATION_CHECKOUT";
    if let Some(primary) = std::env::var_os(PRIMARY) {
        let checkout = std::env::var_os(CHECKOUT).unwrap();
        let primary = Path::new(&primary);
        let checkout = Path::new(&checkout);
        let host = wrix_state::host::Host::discover().unwrap();
        let original = wrix_state::context::Context::inspect(&host, primary).unwrap();
        let former = wrix_state::context::Context::inspect(&host, checkout).unwrap();
        wrix_state::store::Store::associate(&host, primary, checkout, original.key()).unwrap();
        assert!(original.recheck(&host).is_err());
        assert!(former.recheck(&host).is_err());
        assert!(wrix_state::context::Context::inspect(&host, checkout).is_ok());
        return;
    }
    let fixture = Fixture::new();
    fixture.initialize();
    let primary = fixture.repository("primary");
    let checkout = fixture.repository("checkout");
    association::retain(&fixture, &primary);
    association::retain(&fixture, &checkout);
    let executable = std::env::current_exe().unwrap();
    let child = if fixture.isolated {
        super::fixture::isolation::install_executable(
            fixture.root.path(),
            &executable,
            Path::new("/bin/association-probe"),
        );
        Path::new("/bin/association-probe")
    } else {
        &executable
    };
    let name = std::thread::current().name().unwrap().to_owned();
    let output = super::fixture::isolation::command(
        fixture.root.path(),
        fixture.isolated,
        child,
        &["--exact", &name, "--nocapture"],
    )
    .env(PRIMARY, primary)
    .env(CHECKOUT, &checkout)
    .output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    assert!(association::inspect(&fixture, &checkout).status.success());
}
