//! Actual host workspace state and admission entrypoints.

#![cfg(test)]

#[path = "workspace/admission_fixture.rs"]
mod admission_fixture;
#[path = "workspace/fixture.rs"]
mod fixture;
#[path = "workspace/hardening.rs"]
mod hardening;
#[path = "workspace/resolver.rs"]
mod resolver;

use fixture::{Fixture, snapshot};
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn integrated_admission_preserves_caller_and_primary_scope_and_is_idempotent() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let primary = fixture.repository("primary");
    fs::create_dir(fixture.root.path().join("checkout/subdirectory")).unwrap();
    fixture.adapter(&checkout, &primary, "workspace-one");
    let caller = checkout.join("subdirectory");
    let before = snapshot(&fixture.state());
    assert!(
        !fixture
            .run(&["inspect", "--checkout", caller.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(snapshot(&fixture.state()), before);
    let output = fixture.admit(&caller, "workspace-one", &["--allocate"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let before = snapshot(&fixture.state());
    assert!(
        fixture
            .admit(&caller, "workspace-one", &["--allocate"])
            .status
            .success()
    );
    let output = fixture.run(&["inspect", "--checkout", caller.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rendered = String::from_utf8(output.stdout).unwrap();
    assert!(rendered.contains(&format!("caller: {}", caller.display())));
    assert!(rendered.contains(&format!("scope: {}", primary.display())));
    assert_eq!(snapshot(&fixture.state()), before);
    assert!(!fixture.state().join("workspaces").exists());
}

#[test]
fn disabling_integration_does_not_release_a_native_key() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    fixture.adapter(&checkout, &checkout, "workspace-one");
    assert!(
        fixture
            .admit(&checkout, "workspace-one", &["--allocate"])
            .status
            .success()
    );
    assert!(fixture.run(&["integration", "disable"]).status.success());
    assert!(
        fixture
            .run(&["acknowledge-scope", "--version", "1"])
            .status
            .success()
    );
    let before = snapshot(&fixture.state());
    let output = fixture.run(&["inspect", "--checkout", checkout.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reservation"));
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn changed_primary_requires_readmission_and_retains_the_key() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    fixture.adapter(&checkout, &checkout, "workspace-one");
    let initial = fixture.admit(&checkout, "workspace-one", &["--allocate"]);
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    fs::rename(
        fixture.root.path().join("checkout"),
        fixture.root.path().join("moved"),
    )
    .unwrap();
    let moved = fixture.inside("moved");
    fixture.adapter(&moved, &moved, "workspace-one");
    assert!(
        !fixture
            .run(&["inspect", "--checkout", moved.to_str().unwrap()])
            .status
            .success()
    );
    let retried = fixture.admit(&moved, "workspace-one", &["--allocate"]);
    assert!(
        retried.status.success(),
        "{}",
        String::from_utf8_lossy(&retried.stderr)
    );
    assert_eq!(initial.stdout, retried.stdout);
    assert!(
        fixture
            .run(&["inspect", "--checkout", moved.to_str().unwrap()])
            .status
            .success()
    );
}

#[test]
fn malformed_attestation_cannot_change_admission_state() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    for body in [
        "printf '%s' '{}'",
        "printf '%s' '{\"version\":1,\"version\":1}'",
        "printf '%s' '[]'",
        "exit 1",
        "printf '%s' '{} {}'",
    ] {
        fixture.adapter_script(&format!(
            "IFS= read -r request || [[ -n \"$request\" ]]\n{body}"
        ));
        let before = snapshot(&fixture.state());
        assert!(
            !fixture
                .admit(&checkout, "workspace-one", &["--allocate"])
                .status
                .success()
        );
        assert_eq!(snapshot(&fixture.state()), before);
    }
}

#[test]
fn adapter_execution_has_no_repository_working_directory_or_inherited_git_overrides() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let response = serde_json::json!({"version":1, "membership":"workspace-one", "checkout_root_b64":wrix_state::resolver::encode(&checkout), "primary_root_b64":wrix_state::resolver::encode(&checkout)});
    fixture.adapter_script(&format!("IFS= read -r request || [[ -n \"$request\" ]]\n[[ $PWD = / && $HOME = / && -z ${{GIT_DIR+x}} && -z ${{SECRET+x}} ]]\nprintf '%s' '{response}'"));
    let output = fixture
        .command(&[
            "admit",
            "--integration",
            "test-manager",
            "--expect-membership",
            "workspace-one",
            "--checkout",
            checkout.to_str().unwrap(),
            "--allocate",
        ])
        .env("GIT_DIR", "/unrelated")
        .env("SECRET", "must-not-be-inherited")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ordinary_service_cache_and_init_refuse_unknown_membership_before_effects() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    fixture.adapter(&checkout, &checkout, "workspace-one");
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(fixture.root.path().join("checkout"))
            .args([
                "config",
                "remote.origin.url",
                "git@github.com:example/fixture.git"
            ])
            .status()
            .unwrap()
            .success()
    );
    let before = snapshot(&fixture.state());
    let git_before = snapshot(&fixture.root.path().join("checkout/.git"));
    for args in [
        vec!["service", "start"],
        vec!["service", "cache", "prune"],
        vec![
            "init",
            "repo",
            "--offline",
            "--no-hooks",
            "--no-sign",
            "--key",
            "example",
        ],
    ] {
        let output = fixture.application(&checkout, &args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("admission"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(snapshot(&fixture.state()), before);
        assert_eq!(
            snapshot(&fixture.root.path().join("checkout/.git")),
            git_before
        );
    }
}

#[test]
fn admitted_service_uses_primary_scope_and_retains_key_after_move() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let primary = fixture.repository("primary");
    fixture.adapter(&checkout, &primary, "workspace-one");
    assert!(
        fixture
            .admit(&checkout, "workspace-one", &["--allocate"])
            .status
            .success()
    );
    let initial = fixture.application(&checkout, &["service", "endpoints"]);
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let initial: serde_json::Value = serde_json::from_slice(&initial.stdout).unwrap();
    assert_eq!(initial["workspace_path"], primary.to_str().unwrap());
    fs::rename(
        fixture.root.path().join("primary"),
        fixture.root.path().join("moved"),
    )
    .unwrap();
    let moved = fixture.inside("moved");
    fixture.adapter(&checkout, &moved, "workspace-one");
    assert!(
        !fixture
            .application(&checkout, &["service", "endpoints"])
            .status
            .success()
    );
    assert!(
        fixture
            .admit(&checkout, "workspace-one", &["--allocate"])
            .status
            .success()
    );
    let output = fixture.application(&checkout, &["service", "endpoints"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let changed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(initial["workspace_hash"], changed["workspace_hash"]);
    assert_eq!(changed["workspace_path"], moved.to_str().unwrap());
    assert!(!fixture.state().join("workspaces").exists());
}

#[test]
fn native_import_is_explicit_and_retention_does_not_claim_old_checkout_provenance() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let key = "a".repeat(64);
    let native = fixture.state().join("workspaces").join(&key);
    fs::create_dir_all(&native).unwrap();
    let services = serde_json::json!({"schema_version":1, "workspace_hash":key, "workspace_path":"/a/former/location", "container_name":"retained-service", "endpoints":{}});
    fs::write(
        native.join("services.json"),
        serde_json::to_vec(&services).unwrap(),
    )
    .unwrap();
    let original = snapshot(&native);
    fixture.adapter(&checkout, &checkout, "workspace-one");
    assert!(
        !fixture
            .admit(&checkout, "workspace-one", &["--retain", &key])
            .status
            .success()
    );
    assert!(
        fixture
            .run(&["import-native", "--native-key", &key])
            .status
            .success()
    );
    assert_eq!(snapshot(&native), original);
    assert!(
        fixture
            .admit(&checkout, "workspace-one", &["--retain", &key])
            .status
            .success()
    );
    let before = snapshot(&fixture.state());
    let output = fixture.application(&checkout, &["service", "endpoints"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rendered: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rendered["workspace_hash"], key);
    assert_eq!(rendered["container_name"], "retained-service");
    assert_eq!(rendered["workspace_path"], checkout.to_str().unwrap());
    assert_eq!(snapshot(&fixture.state()), before);
    assert!(!fixture.root.path().join("a/former/location").exists());
}

#[test]
fn host_cache_status_uses_database_not_export_files_and_is_read_only() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let key = "b".repeat(64);
    let native = fixture.state().join("workspaces").join(&key);
    fs::create_dir_all(native.join("pending")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(native.join("services.json"), serde_json::to_vec(&serde_json::json!({"schema_version":1,"workspace_hash":key,"container_name":"queue-service","endpoints":{}})).unwrap()).unwrap();
    fs::write(
        native.join("cache-status.json"),
        r#"{"dirty":true,"last_publish":"imported","last_prune":null,"last_error":null}"#,
    )
    .unwrap();
    fs::write(native.join("pending/one.json"), serde_json::to_vec(&serde_json::json!({"created_at_epoch":now,"drv_path":"/nix/store/example.drv","out_paths":[]})).unwrap()).unwrap();
    assert!(
        fixture
            .run(&["import-native", "--native-key", &key])
            .status
            .success()
    );
    fixture.adapter(&checkout, &checkout, "workspace-one");
    let retained = fixture.admit(&checkout, "workspace-one", &["--retain", &key]);
    assert!(
        retained.status.success(),
        "{}",
        String::from_utf8_lossy(&retained.stderr)
    );
    fs::remove_file(native.join("cache-status.json")).unwrap();
    fs::remove_file(native.join("pending/one.json")).unwrap();
    let before = snapshot(&fixture.state());
    let output = fixture.application(&checkout, &["service", "cache", "status"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rendered = String::from_utf8(output.stdout).unwrap();
    assert!(rendered.contains("last_publish: imported"));
    assert!(rendered.contains("pending records: 1"));
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn failed_mutation_cannot_poison_an_empty_host_root() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.state()).unwrap();
    fs::set_permissions(fixture.state(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!fixture.run(&["integration", "disable"]).status.success());
    assert!(snapshot(&fixture.state()).is_empty());
    fixture.initialize();
}
