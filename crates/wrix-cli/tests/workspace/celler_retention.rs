use super::{Fixture, legacy_services, snapshot};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};
use wrix_core::path::WorkspaceHash;

fn generate(keys: &Path, name: &str) {
    fs::create_dir_all(keys).unwrap();
    let output = Command::new("nix-store")
        .args(["--generate-binary-cache-key", name])
        .arg(keys.join("cache.secret"))
        .arg(keys.join("cache.pub"))
        .output()
        .unwrap();
    assert!(output.status.success(), "Nix fixture key generation failed");
    for name in ["cache.secret", "cache.pub"] {
        fs::set_permissions(keys.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn retention_preserves_stale_legacy_keys_identity_payload_and_membership_reservations() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let key = WorkspaceHash::parse(&"c".repeat(64)).unwrap();
    let native = legacy_services(&fixture, &key, "retained-service");
    generate(&native.join("keys"), "legacy");
    let current = tempfile::tempdir().unwrap();
    generate(current.path(), "current-native-view");
    fs::copy(
        current.path().join("cache.pub"),
        native.join("keys/cache.pub"),
    )
    .unwrap();
    fs::write(native.join("retained-payload"), "must survive retention").unwrap();
    let native_before = snapshot(&native);
    assert!(
        fixture
            .run(&["import-native", "--native-key", key.as_str()])
            .status
            .success()
    );
    fixture.adapter(&checkout, &checkout, "workspace-one");
    for _ in 0..2 {
        let output = fixture.admit(&checkout, "workspace-one", &["--retain", key.as_str()]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            snapshot(&native).eq(&native_before),
            "retention changed native data"
        );
    }
    let diagnosed = fixture.run(&["diagnose", "--native-key", key.as_str()]);
    assert!(
        diagnosed.status.success(),
        "{}",
        String::from_utf8_lossy(&diagnosed.stderr)
    );
    let report = String::from_utf8(diagnosed.stdout).unwrap();
    assert!(report.contains("legacy_cache_secret_present: true"));
    assert!(report.contains("active_celler_key: not checked"));
    let before = snapshot(&fixture.state());
    fixture.adapter(&checkout, &checkout, "conflicting-membership");
    let output = fixture.admit(
        &checkout,
        "conflicting-membership",
        &["--retain", key.as_str()],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("conflicting"));
    assert!(
        snapshot(&fixture.state()).eq(&before),
        "conflict changed authority or namespace data"
    );
}

#[test]
fn failed_retention_preserves_committed_integration_selection_and_authority() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let key = WorkspaceHash::parse(&"d".repeat(64)).unwrap();
    let native = legacy_services(&fixture, &key, "retained-service");
    generate(&native.join("keys"), "legacy");
    fs::write(
        native.join("keys/cache.pub"),
        "malformed public observation",
    )
    .unwrap();
    assert!(
        fixture
            .run(&["import-native", "--native-key", key.as_str()])
            .status
            .success()
    );
    fixture.adapter(&checkout, &checkout, "workspace-one");
    let before = snapshot(&fixture.state());
    let output = fixture.admit(&checkout, "workspace-one", &["--retain", key.as_str()]);
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("keys/cache.pub is malformed"), "{error}");
    assert!(!error.contains("not initialized"), "{error}");
    assert!(
        snapshot(&fixture.state()).eq(&before),
        "failed retention changed committed state"
    );
    let report = fixture.run(&["diagnose"]);
    assert!(report.status.success());
    let report = String::from_utf8(report.stdout).unwrap();
    assert!(report.contains("integration: test-manager"), "{report}");
    assert!(
        report.contains("authority_generation_bytes: 32"),
        "{report}"
    );
    let db = rusqlite::Connection::open_with_flags(
        fixture.state().join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let bindings: i64 = db
        .query_row("SELECT COUNT(*) FROM binding", [], |row| row.get(0))
        .unwrap();
    assert_eq!(bindings, 0);
}

#[test]
fn missing_required_public_observation_is_distinct_and_does_not_reset_authority() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let key = WorkspaceHash::parse(&"f".repeat(64)).unwrap();
    let native = legacy_services(&fixture, &key, "retained-service");
    fs::write(
        native.join("services.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "workspace_hash": key.as_str(),
            "container_name": "retained-service",
            "endpoints": {"cache_http": {"host": "127.0.0.1", "port": 5000}}
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        fixture
            .run(&["import-native", "--native-key", key.as_str()])
            .status
            .success()
    );
    fixture.adapter(&checkout, &checkout, "workspace-one");
    let before = snapshot(&fixture.state());
    let output = fixture.admit(&checkout, "workspace-one", &["--retain", key.as_str()]);
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("retained namespace requires keys/cache.pub"),
        "{error}"
    );
    assert!(!error.contains("initialize explicitly"), "{error}");
    assert!(snapshot(&fixture.state()).eq(&before));
}

#[test]
fn diagnose_distinguishes_xdg_roots_without_inferring_or_recreating_selection() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    fixture.adapter(&checkout, &checkout, "workspace-one");
    let before = snapshot(&fixture.state());
    let selected = fixture.run(&["diagnose"]);
    assert!(selected.status.success());
    let report = String::from_utf8(selected.stdout).unwrap();
    assert!(report.contains("integration: test-manager"));
    assert!(report.contains("HOME: "));
    assert!(report.contains("XDG_STATE_HOME: "));
    assert!(report.contains("executable: "));
    let other = fixture
        .command(&["diagnose"])
        .env("XDG_STATE_HOME", "/different-state-root")
        .output()
        .unwrap();
    assert!(other.status.success());
    let report = String::from_utf8(other.stdout).unwrap();
    if cfg!(target_os = "macos") {
        assert!(report.contains("integration: test-manager"), "{report}");
    } else {
        assert!(
            report.contains("database: /different-state-root/wrix/state.db"),
            "{report}"
        );
        assert!(report.contains("database absent"), "{report}");
        assert!(!fixture.root.path().join("different-state-root").exists());
    }
    assert!(snapshot(&fixture.state()).eq(&before));
}

#[test]
fn diagnose_is_passive_and_distinguishes_missing_expected_authority() {
    let fixture = Fixture::new();
    let before = snapshot(fixture.root.path());
    let output = fixture.run(&["diagnose"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("database absent")
    );
    assert!(snapshot(fixture.root.path()).eq(&before));
    fixture.initialize();
    fs::remove_file(fixture.state().join("state.db")).unwrap();
    let before = snapshot(&fixture.state());
    for args in [vec!["diagnose"], vec!["initialize"]] {
        let output = fixture.run(&args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("expected host state.db is missing")
        );
        assert!(snapshot(&fixture.state()).eq(&before));
    }
}

#[test]
fn celler_public_only_namespace_retains_without_a_host_signing_secret() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let key = WorkspaceHash::parse(&"e".repeat(64)).unwrap();
    let native = legacy_services(&fixture, &key, "retained-service");
    let current = tempfile::tempdir().unwrap();
    generate(current.path(), "current-native-view");
    fs::create_dir(native.join("keys")).unwrap();
    fs::copy(
        current.path().join("cache.pub"),
        native.join("keys/cache.pub"),
    )
    .unwrap();
    assert!(
        fixture
            .run(&["import-native", "--native-key", key.as_str()])
            .status
            .success()
    );
    fixture.adapter(&checkout, &checkout, "workspace-one");
    let before = snapshot(&native);
    let output = fixture.admit(&checkout, "workspace-one", &["--retain", key.as_str()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(snapshot(&native).eq(&before));
    assert!(!native.join("keys/cache.secret").exists());
}
