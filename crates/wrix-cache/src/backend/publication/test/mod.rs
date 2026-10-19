use std::{fs, os::unix::fs::PermissionsExt};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;

struct Fixture {
    temporary: TempDir,
    issuer: PathBuf,
    control: PathBuf,
    workspace: WorkspaceHash,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let issuer = temporary.path().join("issuer");
        let metadata = temporary.path().join("metadata");
        fs::create_dir(&metadata).unwrap();
        super::super::issuer::Authority::provision(&issuer, &metadata).unwrap();
        let control = temporary.path().join("control");
        let workspace = wrix_core::path::Workspace::from_path(temporary.path())
            .unwrap()
            .hash()
            .clone();
        Self {
            temporary,
            issuer,
            control,
            workspace,
        }
    }

    fn install(&self) -> Result<()> {
        Publisher::install_issuer(
            &self.control,
            &self.workspace,
            "http://127.0.0.1:1/",
            CacheName::new("private".into()).unwrap(),
            &self.issuer,
            Duration::from_hours(720),
        )
    }
}

#[test]
fn issuer_binding_persists_only_private_issuance_coordinates() {
    let fixture = Fixture::new();
    fixture.install().unwrap();
    let path = fixture.control.join("celler-publisher.json");
    let binding: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(binding["schema_version"], 2);
    assert_eq!(binding["issuer_root"], json!(fixture.issuer));
    assert_eq!(binding["namespace"], "private");
    assert!(binding.get("token").is_none());
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        fs::metadata(&fixture.control).unwrap().mode() & 0o777,
        0o700
    );
    Publisher::load(&fixture.control, &fixture.workspace).unwrap();
    assert_eq!(
        fs::read_dir(fixture.temporary.path().join("metadata"))
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
async fn loaded_publisher_reopens_issuer_for_every_operation() {
    let fixture = Fixture::new();
    fixture.install().unwrap();
    let publisher = Publisher::load(&fixture.control, &fixture.workspace).unwrap();
    fs::remove_file(fixture.issuer.join("issuer.pem")).unwrap();
    assert!(matches!(publisher.register().await, Err(Error::Issuer(_))));
    assert!(matches!(
        publisher.publish_selected(&[]).await,
        Err(Error::Issuer(_))
    ));
    assert!(matches!(publisher.rotate().await, Err(Error::Issuer(_))));
    assert!(!fixture.issuer.join("issuer.pem").exists());
}

#[test]
fn ambiguous_and_cross_version_authority_cannot_fall_back_to_a_token() {
    let fixture = Fixture::new();
    fixture.install().unwrap();
    let path = fixture.control.join("celler-publisher.json");
    let binding: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for (version, token, root) in [
        (1, None, Some(fixture.issuer.clone())),
        (1, Some("fallback"), Some(fixture.issuer.clone())),
        (2, Some("fallback"), Some(fixture.issuer.clone())),
        (2, Some("fallback"), None),
        (2, None, None),
        (3, None, Some(fixture.issuer.clone())),
    ] {
        let mut invalid = binding.clone();
        invalid["schema_version"] = json!(version);
        invalid["token"] = json!(token);
        invalid["issuer_root"] = json!(root);
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(matches!(
            Publisher::load(&fixture.control, &fixture.workspace),
            Err(Error::Binding)
        ));
    }
}

#[test]
fn failed_issuer_install_preserves_previous_binding_and_other_authority() {
    let fixture = Fixture::new();
    fixture.install().unwrap();
    let path = fixture.control.join("celler-publisher.json");
    let previous = fs::read(&path).unwrap();
    fs::set_permissions(
        fixture.issuer.join("issuer.pem"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(matches!(fixture.install(), Err(Error::Issuer(_))));
    assert_eq!(fs::read(&path).unwrap(), previous);
    assert!(Publisher::load(&fixture.control, &fixture.workspace).is_err());
    assert_eq!(fs::read_dir(&fixture.control).unwrap().count(), 1);
}
