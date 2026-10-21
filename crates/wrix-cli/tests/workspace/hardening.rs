#[path = "retention.rs"]
mod retention;

use super::{Fixture, snapshot};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
use wrix_core::path::WorkspaceHash;

fn legacy_services(fixture: &Fixture, key: &WorkspaceHash, name: &str) -> std::path::PathBuf {
    let root = fixture.state().join("workspaces").join(key.as_str());
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(fixture.state(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("services.json"), serde_json::to_vec(&serde_json::json!({"schema_version":1,"workspace_hash":key.as_str(),"container_name":name,"endpoints":{}})).unwrap()).unwrap();
    root
}

fn launch_profile(fixture: &Fixture) -> std::path::PathBuf {
    let config = serde_json::json!({"schema":1,"profile":{"name":"fixture"},"image":{"ref":"localhost/fixture:latest","source":"/missing/source","source_kind":if cfg!(target_os="macos") {"docker-archive"} else {"nix-descriptor"},"digest":format!("sha256:{}", "a".repeat(64))},"agent":{"kind":"direct"},"services":{"nix_cache":{"enable":false}}});
    fs::write(
        fixture.root.path().join("profile.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    fixture.inside("profile.json")
}

#[test]
fn worker_launch_refuses_to_expose_the_selected_resolver_from_the_checkout() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    fixture.adapter(&checkout, &checkout, "workspace-one");
    fs::rename(
        fixture.root.path().join("adapter"),
        fixture.root.path().join("checkout/adapter"),
    )
    .unwrap();
    assert!(
        fixture
            .run(&[
                "integration",
                "select",
                "--slot",
                "test-manager",
                "--executable",
                checkout.join("adapter").to_str().unwrap()
            ])
            .status
            .success()
    );
    assert!(
        fixture
            .admit(&checkout, "workspace-one", &["--allocate"])
            .status
            .success()
    );
    let profile = launch_profile(&fixture);
    let before = snapshot(&fixture.state());
    let output = fixture.application(
        &checkout,
        &[
            "--profile-config",
            profile.to_str().unwrap(),
            "run",
            checkout.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("host-selected workspace resolver"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(snapshot(&fixture.state()), before);
    assert!(!fixture.root.path().join("checkout/.wrix/log").exists());
}

#[test]
fn initialization_does_not_import_existing_native_metadata_or_acknowledge_scope() {
    let fixture = Fixture::new();
    let checkout = fixture.repository("checkout");
    let key = WorkspaceHash::from_path(&checkout);
    let native = legacy_services(&fixture, &key, "checkout-service");
    let before = snapshot(&native);
    fixture.initialize();
    let output = fixture.application(&checkout, &["service", "endpoints"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("import-native"));
    assert_eq!(snapshot(&native), before);
    assert!(
        fixture
            .run(&["import-native", "--native-key", key.as_str()])
            .status
            .success()
    );
    assert_eq!(snapshot(&native), before);
}

#[test]
fn collision_checks_use_recorded_services_after_export_deletion() {
    let fixture = Fixture::new();
    fixture.initialize();
    let first = fixture.repository("a/project");
    let second = fixture.repository("b/project");
    let key = WorkspaceHash::from_path(&first);
    let native = legacy_services(&fixture, &key, "project-service");
    assert!(
        fixture
            .run(&["import-native", "--native-key", key.as_str()])
            .status
            .success()
    );
    fs::remove_file(native.join("services.json")).unwrap();
    let before = snapshot(&fixture.state());
    let output = fixture.application(&second, &["service", "endpoints"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_ne!(value["container_name"], "project-service");
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn concurrent_identical_admissions_confirm_one_key_and_one_binding() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    fixture.adapter(&checkout, &checkout, "workspace-one");
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            fixture
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
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let outputs = children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect::<Vec<_>>();
    for output in &outputs {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, outputs[0].stdout);
    }
    let db = rusqlite::Connection::open(fixture.state().join("state.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM binding", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn previously_admitted_participant_residue_blocks_primary_operations_and_readmission() {
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
    let competing = fixture
        .state()
        .join("workspaces")
        .join(WorkspaceHash::from_path(&checkout).as_str());
    fs::create_dir_all(&competing).unwrap();
    fs::write(competing.join("sentinel"), "older writer's state").unwrap();
    fixture.adapter(&primary, &primary, "workspace-one");
    let before = snapshot(&fixture.state());
    assert!(
        !fixture
            .run(&["inspect", "--checkout", primary.to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        !fixture
            .admit(&primary, "workspace-one", &["--allocate"])
            .status
            .success()
    );
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn changed_attestation_is_rechecked_before_service_files_are_created() {
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
    let count = fixture.inside("count");
    fs::write(fixture.root.path().join("count"), "0\n").unwrap();
    let reply = serde_json::json!({"version":1,"membership":"workspace-one","checkout_root_b64":wrix_state::resolver::encode(&checkout),"primary_root_b64":wrix_state::resolver::encode(&checkout)});
    let changed = reply.to_string().replace("workspace-one", "workspace-two");
    fixture.adapter_script(&format!("IFS= read -r request || [[ -n \"$request\" ]]\nread -r count < '{}'\ncount=$((count + 1))\nprintf '%s\\n' \"$count\" > '{}'\nif [[ \"$count\" = 1 ]]; then printf '%s' '{reply}'; else printf '%s' '{changed}'; fi", count.display(), count.display()));
    let before = snapshot(&fixture.state());
    let output = fixture.application(&checkout, &["service", "start"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("conflicting workspace"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(snapshot(&fixture.state()), before);
    fs::write(fixture.root.path().join("count"), "0\n").unwrap();
    let profile = launch_profile(&fixture);
    let output = fixture.application(
        &checkout,
        &[
            "--profile-config",
            profile.to_str().unwrap(),
            "run",
            checkout.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("conflicting workspace"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(snapshot(&fixture.state()), before);
    assert!(!fixture.root.path().join("checkout/.wrix/log").exists());
}

#[test]
fn malformed_expected_binding_records_are_not_repaired_by_readmission() {
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
    let db = rusqlite::Connection::open(fixture.state().join("state.db")).unwrap();
    db.execute("UPDATE binding SET primary_evidence = X'00'", [])
        .unwrap();
    drop(db);
    let before = snapshot(&fixture.state());
    assert!(
        !fixture
            .admit(&checkout, "workspace-one", &["--allocate"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["inspect", "--checkout", checkout.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn admitted_beads_push_retains_caller_git_ownership_and_caller_path_guards() {
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
    let git = fixture.root.path().join("checkout/.git");
    let git_before = snapshot(&git);
    let primary_before = snapshot(&fixture.root.path().join("primary/.git"));
    symlink(fixture.inside("foreign"), git.join("beads-worktrees")).unwrap();
    let before = snapshot(&fixture.state());
    let output = fixture.application(&checkout, &["beads", "push"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsafe beads worktree path"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_file(git.join("beads-worktrees")).unwrap();
    assert_eq!(snapshot(&fixture.state()), before);
    assert_eq!(snapshot(&git), git_before);
    assert_eq!(
        snapshot(&fixture.root.path().join("primary/.git")),
        primary_before
    );
}

#[test]
fn worker_mount_planning_refuses_host_state_ancestors_and_aliases_before_staging() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    symlink(fixture.inside("home"), fixture.root.path().join("alias")).unwrap();
    let profile = fixture.inside("profile.json");
    for source in [fixture.inside("home"), fixture.inside("alias")] {
        let config = serde_json::json!({"schema":1,"profile":{"name":"fixture","mounts":[{"source":source,"dest":"/mnt/broad","mode":"rw"}]},"image":{"ref":"localhost/fixture:latest","source":"/missing/source","source_kind":if cfg!(target_os="macos") {"docker-archive"} else {"nix-descriptor"},"digest":format!("sha256:{}", "a".repeat(64))},"agent":{"kind":"direct"},"services":{"nix_cache":{"enable":false}}});
        fs::write(
            fixture.root.path().join("profile.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let before = snapshot(&fixture.state());
        for preview in [false, true] {
            let mut args = vec!["--profile-config", profile.to_str().unwrap(), "run"];
            if preview {
                args.push("--dry-run");
            }
            args.push(checkout.to_str().unwrap());
            let output = fixture.application(&checkout, &args);
            assert!(!output.status.success());
            if preview {
                assert!(String::from_utf8_lossy(&output.stdout).contains("invalid_input"));
            } else {
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("private Wrix host state"),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        assert_eq!(snapshot(&fixture.state()), before);
        assert!(!fixture.root.path().join("home/.cache/wrix/mounts").exists());
    }
}
