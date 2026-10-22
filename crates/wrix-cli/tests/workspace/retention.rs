use super::{Fixture, legacy_services, snapshot};
use std::fs;
use wrix_core::path::WorkspaceHash;

#[test]
fn unsupported_flat_cache_prune_preserves_imported_closures_and_keys() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let key = WorkspaceHash::from_path(&checkout);
    let native = legacy_services(&fixture, &key, "checkout-service");
    let keys = native.join("keys");
    fs::create_dir_all(&keys).unwrap();
    let generated = std::process::Command::new("nix-store")
        .arg("--generate-binary-cache-key")
        .arg("fixture")
        .arg(keys.join("cache.secret"))
        .arg(keys.join("cache.pub"))
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let keys_before = snapshot(&keys);
    fs::create_dir(native.join("gcroots")).unwrap();
    let hash = "a".repeat(32);
    fs::write(
        native.join("gcroots/project"),
        format!("/nix/store/{hash}-retained\n"),
    )
    .unwrap();
    assert!(
        fixture
            .run(&["import-native", "--native-key", key.as_str()])
            .status
            .success()
    );
    fs::remove_file(native.join("gcroots/project")).unwrap();
    let cache = if cfg!(target_os = "macos") {
        fixture.root.path().join("home/Library/Caches/wrix")
    } else {
        fixture.root.path().join("home/.cache/wrix")
    }
    .join("workspaces")
    .join(key.as_str())
    .join("binary-cache");
    fs::create_dir_all(cache.join("nar")).unwrap();
    fs::write(
        cache.join(format!("{hash}.narinfo")),
        "URL: nar/retained.nar\n",
    )
    .unwrap();
    fs::write(cache.join("nar/retained.nar"), "retained payload").unwrap();
    let output = fixture.application(&checkout, &["service", "cache", "prune"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown cache command: prune"));
    assert_eq!(
        fs::read_to_string(cache.join("nar/retained.nar")).unwrap(),
        "retained payload"
    );
    assert_eq!(snapshot(&keys), keys_before);
    assert!(!native.join("gcroots/project").exists());
}

#[test]
fn native_key_and_participant_reservations_refuse_forward_reverse_and_cross_slot_conflicts() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let primary = fixture.repository("primary");
    let other = fixture.repository("other");
    let key = WorkspaceHash::from_path(&primary);
    legacy_services(&fixture, &key, "primary-service");
    assert!(
        fixture
            .run(&["import-native", "--native-key", key.as_str()])
            .status
            .success()
    );
    fixture.adapter(&checkout, &primary, "workspace-one");
    assert!(
        fixture
            .admit(&checkout, "workspace-one", &["--retain", key.as_str()])
            .status
            .success()
    );
    fixture.adapter(&checkout, &other, "workspace-two");
    let before = snapshot(&fixture.state());
    assert!(
        !fixture
            .admit(&checkout, "workspace-two", &["--allocate"])
            .status
            .success()
    );
    assert_eq!(snapshot(&fixture.state()), before);
    for slot in ["test-manager", "other-manager"] {
        fixture.adapter(&other, &primary, "workspace-two");
        assert!(
            fixture
                .run(&[
                    "integration",
                    "select",
                    "--slot",
                    slot,
                    "--executable",
                    fixture.inside("adapter").to_str().unwrap()
                ])
                .status
                .success()
        );
        let before = snapshot(&fixture.state());
        let output = fixture.run(&[
            "admit",
            "--integration",
            slot,
            "--expect-membership",
            "workspace-two",
            "--checkout",
            other.to_str().unwrap(),
            "--retain",
            key.as_str(),
        ]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("conflicting"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(snapshot(&fixture.state()), before);
    }
}
