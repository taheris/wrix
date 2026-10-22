use super::{Fixture, snapshot};
use std::{fs, path::Path};

fn setup() -> (Fixture, std::path::PathBuf) {
    let fixture = Fixture::new();
    fixture.initialize();
    assert!(
        fixture
            .run(&["acknowledge-scope", "--version", "1"])
            .status
            .success()
    );
    let checkout = fixture.repository("checkout");
    (fixture, checkout)
}

fn deployment(fixture: &Fixture) {
    let root = fixture.state().join("control/authority");
    fs::create_dir_all(root.parent().unwrap()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        wrix_service::control::Controller::bind(&root)
            .unwrap()
            .close()
            .unwrap();
    });
}

fn invoke(fixture: &Fixture, checkout: &Path, arguments: &[&str]) -> std::process::Output {
    let mut args = vec!["service", "nix-store"];
    args.extend_from_slice(arguments);
    fixture.application(checkout, &args)
}

fn success(fixture: &Fixture, checkout: &Path, arguments: &[&str]) -> String {
    let output = invoke(fixture, checkout, arguments);
    assert!(
        output.status.success(),
        "{checkout:?} {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn repository(output: &str) -> &str {
    output
        .lines()
        .find_map(|line| line.strip_prefix("canonical repository: "))
        .unwrap()
}

#[test]
fn installed_store_information_is_passive_and_enable_never_creates_a_deployment() {
    let (fixture, checkout) = setup();
    let before = snapshot(&fixture.state());
    assert!(success(&fixture, &checkout, &[]).contains("approval: unapproved"));
    assert_eq!(snapshot(&fixture.state()), before);
    let enabled = invoke(&fixture, &checkout, &["enable"]);
    assert!(!enabled.status.success());
    assert!(String::from_utf8_lossy(&enabled.stderr).contains("no recorded host deployment"));
    assert_eq!(snapshot(&fixture.state()), before);
    assert!(!fixture.state().join("control").exists());
}

#[test]
fn approval_is_database_owned_without_importing_filesystem_claims_or_mutating_payloads() {
    let (fixture, checkout) = setup();
    deployment(&fixture);
    let enabled = success(&fixture, &checkout, &["enable"]);
    assert!(enabled.contains("approval: enabled"));
    let key = wrix_core::path::WorkspaceHash::from_path(&checkout);
    let native = fixture.state().join("workspaces").join(key.as_str());
    fs::create_dir_all(&native).unwrap();
    let legacy = b"{\"enabled\":true,\"repository\":\"forged\"}";
    fs::write(native.join("nix-store.json"), legacy).unwrap();
    fs::write(native.join("retained-payload"), b"do not delete").unwrap();
    let disabled = success(&fixture, &checkout, &["disable"]);
    assert!(disabled.contains("approval: disabled"));
    assert_eq!(repository(&enabled), repository(&disabled));
    assert_eq!(fs::read(native.join("nix-store.json")).unwrap(), legacy);
    assert_eq!(
        fs::read(native.join("retained-payload")).unwrap(),
        b"do not delete"
    );
    let before = snapshot(&fixture.state());
    assert!(success(&fixture, &checkout, &[]).contains("approval: disabled"));
    assert_eq!(snapshot(&fixture.state()), before);
    assert!(!fixture.state().join("service-data/nix").exists());
}

#[test]
fn linked_checkouts_share_one_repository_only_after_explicit_membership_and_disable_together() {
    let (fixture, checkout) = setup();
    deployment(&fixture);
    assert!(
        fixture
            .git(
                &checkout,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.test",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "initial"
                ]
            )
            .status
            .success()
    );
    let linked = fixture.inside("linked");
    assert!(
        fixture
            .git(
                &checkout,
                &["worktree", "add", "--detach", linked.to_str().unwrap()]
            )
            .status
            .success()
    );
    let first = success(&fixture, &checkout, &["enable"]);
    let before = snapshot(&fixture.state());
    assert!(success(&fixture, &linked, &[]).contains("approval: unapproved"));
    assert_eq!(snapshot(&fixture.state()), before);
    let second = success(&fixture, &linked, &["enable"]);
    assert_eq!(repository(&first), repository(&second));
    let peer = fixture.repository("peer");
    let unrelated = success(&fixture, &peer, &["enable"]);
    assert_ne!(repository(&first), repository(&unrelated));
    assert!(success(&fixture, &linked, &["disable"]).contains("approval: disabled"));
    assert!(success(&fixture, &checkout, &[]).contains("approval: disabled"));
    assert!(success(&fixture, &peer, &[]).contains("approval: enabled"));
    let again = success(&fixture, &checkout, &["enable"]);
    assert_eq!(repository(&first), repository(&again));
    assert!(success(&fixture, &linked, &[]).contains("approval: enabled"));
}

#[test]
fn admitted_clones_use_the_checked_primary_repository_without_path_based_grouping() {
    let (fixture, checkout) = setup();
    deployment(&fixture);
    let primary = fixture.repository("primary");
    fixture.adapter(&checkout, &primary, "member");
    assert!(
        fixture
            .admit(&checkout, "member", &["--allocate"])
            .status
            .success()
    );
    let first = success(&fixture, &checkout, &["enable"]);
    let other = fixture.repository("other");
    fixture.adapter(&other, &primary, "member");
    let before = snapshot(&fixture.state());
    assert!(!invoke(&fixture, &other, &["enable"]).status.success());
    assert_eq!(snapshot(&fixture.state()), before);
    assert!(
        fixture
            .admit(&other, "member", &["--allocate"])
            .status
            .success()
    );
    assert_eq!(
        repository(&first),
        repository(&success(&fixture, &other, &[]))
    );
}

#[test]
fn malformed_expected_approval_refuses_information_and_mutations_without_repair() {
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let db = rusqlite::Connection::open(fixture.state().join("state.db")).unwrap();
    db.execute(
        "UPDATE namespace_metadata SET value=?1 WHERE owner='installed-store-approval'",
        [b"{\"version\":99,\"enabled\":true}".as_slice()],
    )
    .unwrap();
    let before = snapshot(&fixture.state());
    for args in [
        vec![],
        vec!["enable"],
        vec!["disable"],
        vec!["gc"],
        vec!["enable", "--host"],
    ] {
        assert!(!invoke(&fixture, &checkout, &args).status.success());
        assert_eq!(snapshot(&fixture.state()), before);
    }
}

#[test]
fn inconsistent_repository_decisions_cannot_be_repaired_by_enable() {
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let db = rusqlite::Connection::open(fixture.state().join("state.db")).unwrap();
    let bytes: Vec<u8> = db
        .query_row(
            "SELECT value FROM namespace_metadata WHERE owner='installed-store-approval'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    record["enabled"] = serde_json::json!(false);
    db.execute(
        "INSERT INTO namespace_metadata VALUES (?1, 'installed-store-approval', ?2)",
        rusqlite::params![
            wrix_core::path::WorkspaceHash::from_path(Path::new("/unrelated-record")).as_str(),
            serde_json::to_vec(&record).unwrap()
        ],
    )
    .unwrap();
    let before = snapshot(&fixture.state());
    let output = invoke(&fixture, &checkout, &["enable"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("inconsistent canonical repository"));
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn repository_replacement_is_not_automatic_reapproval() {
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    fs::rename(
        fixture.root.path().join("checkout/.git"),
        fixture.root.path().join("retained-git"),
    )
    .unwrap();
    assert!(fixture.git(&checkout, &["init", "-q"]).status.success());
    let before = snapshot(&fixture.state());
    for args in [vec![], vec!["enable"], vec!["disable"]] {
        let output = invoke(&fixture, &checkout, &args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("repository administration changed"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn moved_repository_requires_reconciliation_instead_of_allocating_another_identity() {
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    fs::rename(
        fixture.root.path().join("checkout"),
        fixture.root.path().join("moved"),
    )
    .unwrap();
    let moved = fixture.inside("moved");
    let before = snapshot(&fixture.state());
    for args in [vec![], vec!["enable"], vec!["disable"]] {
        let output = invoke(&fixture, &moved, &args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("repository administration changed")
        );
        assert_eq!(snapshot(&fixture.state()), before);
    }
}

#[test]
fn repeated_matching_approval_preserves_identity_and_metadata_bytes() {
    let (fixture, checkout) = setup();
    deployment(&fixture);
    let first = success(&fixture, &checkout, &["enable"]);
    let before = snapshot(&fixture.state());
    assert_eq!(success(&fixture, &checkout, &["enable"]), first);
    assert_eq!(snapshot(&fixture.state()), before);
}

#[test]
fn concurrent_repository_enables_choose_one_identity_and_preserve_both_members() {
    let (fixture, checkout) = setup();
    deployment(&fixture);
    assert!(
        fixture
            .git(
                &checkout,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.test",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "initial"
                ]
            )
            .status
            .success()
    );
    let linked = fixture.inside("linked");
    assert!(
        fixture
            .git(
                &checkout,
                &["worktree", "add", "--detach", linked.to_str().unwrap()]
            )
            .status
            .success()
    );
    let args = ["service", "nix-store", "enable"];
    let mut first = fixture.application_command(&checkout, &args);
    let mut second = fixture.application_command(&linked, &args);
    let first = first
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let second = second
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    for child in [first, second] {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let first = success(&fixture, &checkout, &[]);
    let second = success(&fixture, &linked, &[]);
    assert_eq!(repository(&first), repository(&second));
    assert!(first.contains("approval: enabled"));
    assert!(second.contains("approval: enabled"));
}

#[test]
fn approved_shared_preflight_is_passive_but_execution_still_requires_runtime_acceptance() {
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let profile = serde_json::json!({
        "schema": 1,
        "profile": {"name":"base"},
        "image": {"ref":"localhost/never-launched:latest", "source":"/missing/source",
            "source_kind": if cfg!(target_os = "macos") {"docker-archive"} else {"nix-descriptor"},
            "digest":format!("sha256:{}", "a".repeat(64)),
            "shared_nix":{"version":1,"seed_closure":"/nix/store/00000000000000000000000000000000-seed/store-paths"}},
        "agent":{"kind":"direct"}, "services":{"nix_cache":{"enable":true}}
    });
    fs::write(
        fixture.root.path().join("profile.json"),
        serde_json::to_vec(&profile).unwrap(),
    )
    .unwrap();
    let path = fixture.inside("profile.json");
    let before = snapshot(&fixture.state());
    let preview = fixture.application(
        &checkout,
        &[
            "--profile-config",
            path.to_str().unwrap(),
            "run",
            "--nix-store",
            "shared",
            "--dry-run",
            "--json",
        ],
    );
    assert!(!preview.status.success());
    let report: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(report["plan"]["nix_store"]["approval"], "enabled");
    assert_eq!(
        report["plan"]["nix_store"]["blockers"],
        serde_json::json!(["runtime_unavailable"])
    );
    assert_eq!(snapshot(&fixture.state()), before);
    let actual = fixture.application(
        &checkout,
        &[
            "--profile-config",
            path.to_str().unwrap(),
            "run",
            "--nix-store",
            "shared",
        ],
    );
    assert!(!actual.status.success());
    assert!(
        String::from_utf8_lossy(&actual.stderr)
            .contains("Shared installed Nix execution is unavailable")
    );
    assert_eq!(snapshot(&fixture.state()), before);
    assert!(!fixture.root.path().join("checkout/.wrix").exists());
    assert!(!fixture.state().join("service-data/nix").exists());
}

#[test]
#[ignore = "requires the worker chroot fixture; run explicitly inside an image"]
fn worker_marker_cannot_change_repository_approval_with_a_forged_home() {
    let (fixture, checkout) = setup();
    assert!(
        fixture.isolated,
        "this test requires the worker chroot fixture"
    );
    deployment(&fixture);
    let marker = fixture.root.path().join("etc/wrix");
    fs::create_dir_all(&marker).unwrap();
    fs::write(marker.join("image-agent"), "direct\n").unwrap();
    let before = snapshot(&fixture.state());
    for args in [vec![], vec!["enable"], vec!["disable"]] {
        let output = invoke(&fixture, &checkout, &args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("host-only"));
    }
    assert_eq!(snapshot(&fixture.state()), before);
}
