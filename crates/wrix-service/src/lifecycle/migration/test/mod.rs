use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    root: tempfile::TempDir,
    runtime: Runtime,
    containers: Value,
}

impl Fixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
        let binary = root.path().join("podman-alias");
        fs::write(&binary, include_str!("runtime.py"))?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
        let mut containers = json!({});
        for (name, workspace, running) in
            [("legacy-a", "repo-a", true), ("legacy-b", "repo-b", false)]
        {
            let workspace = root.path().join(workspace);
            fs::create_dir(&workspace)?;
            containers[name] = json!({
                "id": if name == "legacy-a" { "a".repeat(64) } else { "b".repeat(64) },
                "running": running,
                "labels": {"wrix.kind": "service", "wrix.workspace": workspace, "wrix.workspace.hash": name, "wrix.service.supervision": "1"},
                "mounts": [{"Source": workspace.join(".beads/dolt"), "Destination": "/var/lib/wrix/beads/dolt", "RW": true}]
            });
        }
        let fixture = Self {
            runtime: Runtime {
                binary: binary.to_str().unwrap().into(),
                kind: RuntimeKind::Podman,
                binding: None,
            },
            root,
            containers,
        };
        fixture.save()?;
        fs::create_dir(fixture.root.path().join("host-lifecycle"))?;
        fs::set_permissions(
            fixture.root.path().join("host-lifecycle"),
            fs::Permissions::from_mode(0o700),
        )?;
        Ok(fixture)
    }

    fn save(&self) -> TestResult {
        fs::write(
            self.root.path().join("runtime.json"),
            serde_json::to_vec(&self.containers)?,
        )?;
        Ok(())
    }

    fn inventory(&self) -> Result<(Vec<Entry>, Vec<String>)> {
        inventory_backends(&[&self.runtime], &self.root.path().join("host-lifecycle"))
    }

    fn journal(&self) -> Result<Journal> {
        Ok(Journal {
            version: 1,
            authorized: false,
            entries: self.inventory()?.0,
            consumers: self.inventory()?.1,
            verified: Vec::new(),
        })
    }
}

#[test]
fn runtime_fixture_obeys_native_running_stopped_missing_and_read_only_contract() -> TestResult {
    let fixture = Fixture::new()?;
    for (container, expected) in [
        ("legacy-a", RuntimeStatus::Running),
        ("legacy-b", RuntimeStatus::Stopped),
        ("missing", RuntimeStatus::Missing),
    ] {
        assert_eq!(
            fixture
                .runtime
                .status(&ContainerName::from_persisted(container.into()).unwrap())?,
            expected
        );
    }
    assert!(
        !Command::new(&fixture.runtime.binary)
            .args(["stop", "legacy-a"])
            .output()?
            .status
            .success()
    );
    assert_eq!(fixture.runtime.list_container_infos()?.len(), 2);
    Ok(())
}

#[test]
fn inventory_separates_shared_owner_from_multiple_foreign_repos_and_active_consumers() -> TestResult
{
    let mut fixture = Fixture::new()?;
    fixture.containers["shared"] = fixture.containers["legacy-a"].clone();
    fixture.containers["shared"]["labels"][super::super::shared::LABEL] =
        json!(super::super::shared::VERSION);
    fixture.containers["worker"] = fixture.containers["legacy-a"].clone();
    fixture.containers["worker"]["labels"]["wrix.kind"] = json!("sandbox");
    fixture.save()?;
    let root = fixture.root.path().join("host-lifecycle");
    owner::Selected {
        runtime: RuntimeKind::Podman,
        workspace: None,
        container: ContainerName::from_persisted("shared".into()).unwrap(),
        port: std::num::NonZeroU16::new(4250).unwrap(),
        recipe: None,
    }
    .record(&coordination::acquire(&root)?)?;
    let (entries, consumers) = fixture.inventory()?;
    assert_eq!(entries.len(), 2);
    assert_eq!(consumers.len(), 1);
    assert!(consumers[0].contains("worker") && consumers[0].contains("repo-a"));
    assert_ne!(entries[0].workspace, entries[1].workspace);
    assert!(
        entries
            .iter()
            .all(|entry| entry.binary.ends_with("podman-alias"))
    );
    Ok(())
}

#[test]
fn cutover_requires_every_legacy_service_to_be_stopped() -> TestResult {
    let fixture = Fixture::new()?;
    let journal = fixture.journal()?;
    let error = admit_cutover(&journal, &fixture.inventory()?.0, &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("maintenance approval required") && error.contains("legacy-a"));
    Ok(())
}

#[test]
fn active_consumers_block_even_with_operator_quiescence_claim() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.containers["legacy-a"]["running"] = json!(false);
    fixture.save()?;
    let journal = fixture.journal()?;
    assert!(
        admit_cutover(&journal, &journal.entries, &["host-worker".into()])
            .unwrap_err()
            .to_string()
            .contains("active possible consumers")
    );
    Ok(())
}

#[test]
fn interrupted_cutover_retains_authorization_and_verified_workspace_progress() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.containers["legacy-a"]["running"] = json!(false);
    fixture.save()?;
    let root = fixture.root.path().join("host-lifecycle");
    let mut journal = fixture.journal()?;
    persist(&root, &journal)?;
    assert!(!read(&root)?.unwrap().authorized);
    admit_cutover(&journal, &fixture.inventory()?.0, &[])?;
    journal.authorized = true;
    journal
        .verified
        .push(journal.entries[0].workspace.clone().unwrap());
    persist(&root, &journal)?;
    for _ in 0..2 {
        let saved = read(&root)?.unwrap();
        assert!(saved.authorized);
        assert_eq!(saved.verified, journal.verified);
        admit_cutover(&saved, &fixture.inventory()?.0, &[])?;
    }
    assert_eq!(
        fs::metadata(root.join(JOURNAL))?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}

#[test]
fn stale_launch_recurrence_is_not_authorized_by_retained_migration_intent() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.containers["legacy-a"]["running"] = json!(false);
    fixture.save()?;
    let root = fixture.root.path().join("host-lifecycle");
    let mut journal = fixture.journal()?;
    journal.authorized = true;
    persist(&root, &journal)?;
    let info = fixture
        .runtime
        .list_container_infos()?
        .into_iter()
        .find(|info| info.name == "legacy-a")
        .unwrap();
    assert!(permits(&root, &fixture.runtime, &info)?);
    fixture.containers["legacy-a"]["running"] = json!(true);
    fixture.save()?;
    assert!(!permits(&root, &fixture.runtime, &info)?);
    assert!(admit_cutover(&journal, &fixture.inventory()?.0, &[]).is_err());
    let diagnostic = incompatible(&fixture.runtime, &info)?.to_string();
    for expected in [
        "repo-a",
        "podman-alias",
        "legacy-a",
        "service migrate-legacy",
        "stale shell/package",
    ] {
        assert!(diagnostic.contains(expected), "{diagnostic}");
    }
    assert!(!diagnostic.contains("service recover"));
    Ok(())
}

#[test]
fn recreated_same_named_container_cannot_inherit_migration_authorization() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.containers["legacy-a"]["running"] = json!(false);
    fixture.save()?;
    let mut journal = fixture.journal()?;
    journal.authorized = true;
    let root = fixture.root.path().join("host-lifecycle");
    persist(&root, &journal)?;
    fixture.containers["legacy-a"]["id"] = json!("c".repeat(64));
    fixture.save()?;
    assert!(
        admit_cutover(&journal, &fixture.inventory()?.0, &[])
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    let info = fixture
        .runtime
        .list_container_infos()?
        .into_iter()
        .find(|info| info.name == "legacy-a")
        .unwrap();
    assert!(!permits(&root, &fixture.runtime, &info)?);
    Ok(())
}

#[test]
fn mount_changes_invalidate_prepared_inventory_without_touching_sources() -> TestResult {
    let mut fixture = Fixture::new()?;
    let journal = fixture.journal()?;
    fixture.containers["legacy-a"]["mounts"][0]["Source"] = json!("/other/database");
    fixture.save()?;
    assert!(admit_cutover(&journal, &fixture.inventory()?.0, &[]).is_err());
    Ok(())
}

#[test]
fn verification_does_not_ignore_independent_legacy_history_after_namespace_registration()
-> TestResult {
    let fixture = Fixture::new()?;
    let workspace = fixture.root.path().join("repo-a");
    fs::create_dir_all(workspace.join(".beads/dolt/beads/.dolt/noms"))?;
    fs::write(
        workspace.join(".beads/metadata.json"),
        br#"{"dolt_database":"beads"}"#,
    )?;
    assert!(
        verify_source_retired(&workspace)
            .unwrap_err()
            .to_string()
            .contains("independent history")
    );
    fs::rename(
        workspace.join(".beads/dolt/beads"),
        fixture.root.path().join("retained-checkpoint"),
    )?;
    verify_source_retired(&workspace)?;
    assert!(
        fixture
            .root
            .path()
            .join("retained-checkpoint/.dolt/noms")
            .is_dir()
    );
    Ok(())
}

#[test]
fn verification_without_database_metadata_refuses_unreconciled_sibling_history() -> TestResult {
    let fixture = Fixture::new()?;
    let workspace = fixture.root.path().join("repo-a");
    fs::create_dir_all(workspace.join(".beads/dolt/retained-beads/.dolt/noms"))?;
    assert!(verify_source_retired(&workspace).is_err());
    assert!(
        workspace
            .join(".beads/dolt/retained-beads/.dolt/noms")
            .is_dir()
    );
    Ok(())
}

#[test]
fn runtime_symlink_alias_can_resume_the_same_prepared_inventory() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.containers["legacy-a"]["running"] = json!(false);
    fixture.save()?;
    let journal = fixture.journal()?;
    let alias = fixture.root.path().join("other-podman-name");
    std::os::unix::fs::symlink(&fixture.runtime.binary, &alias)?;
    fixture.runtime.binary = alias.to_str().unwrap().into();
    admit_cutover(&journal, &fixture.inventory()?.0, &[])?;
    Ok(())
}

#[test]
fn migration_guidance_preserves_shell_quoted_runtime_aliases() -> TestResult {
    let mut fixture = Fixture::new()?;
    let alias = fixture.root.path().join("podman's alias");
    std::os::unix::fs::symlink(&fixture.runtime.binary, &alias)?;
    fixture.runtime.binary = alias.to_str().unwrap().into();
    let command = migration_command(&fixture.runtime)?;
    let output = Command::new("sh")
        .args(["-c", &format!("set -- {command}; printf '%s' \"$2\"")])
        .output()?;
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)?,
        format!("WRIX_CONTAINER_RUNTIME={}", alias.display())
    );
    Ok(())
}

#[test]
fn unsafe_or_unknown_journal_is_not_treated_as_absent_intent() -> TestResult {
    let fixture = Fixture::new()?;
    let root = fixture.root.path().join("host-lifecycle");
    assert!(read(&root)?.is_none());
    persist(&root, &fixture.journal()?)?;
    fs::set_permissions(root.join(JOURNAL), fs::Permissions::from_mode(0o644))?;
    assert!(read(&root).is_err());
    fs::set_permissions(root.join(JOURNAL), fs::Permissions::from_mode(0o600))?;
    fs::write(
        root.join(JOURNAL),
        b"{\"version\":2,\"authorized\":true,\"entries\":[],\"consumers\":[],\"verified\":[]}",
    )?;
    assert!(read(&root).is_err());
    Ok(())
}
