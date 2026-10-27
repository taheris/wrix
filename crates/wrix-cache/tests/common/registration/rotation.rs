use std::{fs, os::unix::fs::symlink, path::PathBuf};

use wrix_cache::backend::publication::{
    Error, Publisher,
    registration::{private_namespace, provision_private, rotate_private},
};
use wrix_core::path::WorkspaceHash;

use super::{RETENTION, native};

struct Fixture {
    server: native::Server,
    root: tempfile::TempDir,
    state: PathBuf,
    workspace: WorkspaceHash,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = WorkspaceHash::parse(&"a".repeat(64)).unwrap();
        Self {
            server: native::Server::start_with_issuer(),
            state: root.path().join("control"),
            root,
            workspace,
        }
    }

    async fn provision(&self) -> Result<wrix_core::cache_key::CachePublicKey, Error> {
        provision_private(
            &self.state,
            &self.workspace,
            &self.server.endpoint,
            &self.server.issuer_root(),
            RETENTION,
            std::time::Duration::from_secs(1),
        )
        .await
    }

    async fn rotate(&self) -> Result<wrix_core::cache_key::CachePublicKey, Error> {
        rotate_private(
            &self.state,
            &self.workspace,
            &self.server.endpoint,
            &self.server.issuer_root(),
            RETENTION,
            std::time::Duration::from_secs(1),
        )
        .await
    }

    fn seed_intent(&self, key: &wrix_core::cache_key::CachePublicKey) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.state.join("celler-rotation.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "workspace": self.workspace.as_str(),
                "endpoint": self.server.endpoint,
                "issuer": self.server.issuer_root(),
                "retention_seconds": RETENTION.as_secs(),
                "prior_key": key.as_str(),
            }))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[tokio::test]
async fn recording_failure_reports_partial_rotation_and_retry_preserves_the_replacement_key() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("control");
    let issuer = root.path().join("issuer");
    let metadata = root.path().join("metadata");
    fs::create_dir(&metadata).unwrap();
    wrix_cache::backend::issuer::Authority::provision(&issuer, &metadata).unwrap();
    let workspace = WorkspaceHash::parse(&"a".repeat(64)).unwrap();
    let namespace = private_namespace(&workspace).unwrap();
    let old = "old:11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
    let new = super::PUBLIC_KEY;
    let mut exchanges = vec![
        super::get(super::config(old)),
        super::get(super::config(old)),
    ];
    exchanges.extend(super::rotation_exchanges());
    exchanges.extend([
        super::get(super::config(new)),
        super::get(super::config(new)),
        super::retention_patch(),
        super::get(super::config(new)),
    ]);
    let obstruction = state.join("celler-publisher.json");
    let blocked = obstruction.clone();
    let counter = AtomicUsize::new(0);
    let server = super::Server::start_for_namespace(
        namespace.as_str().to_owned(),
        exchanges,
        move |header| {
            assert!(
                header
                    .to_ascii_lowercase()
                    .contains("authorization: bearer ")
            );
            if header
                .lines()
                .next()
                .unwrap()
                .contains("/_api/v1/cache-config/")
                && counter.fetch_add(1, Ordering::SeqCst) == 6
            {
                fs::create_dir(&blocked).unwrap();
            }
        },
    );
    let rotate = || {
        rotate_private(
            &state,
            &workspace,
            &server.endpoint,
            &issuer,
            RETENTION,
            std::time::Duration::from_secs(1),
        )
    };
    let error = rotate().await.unwrap_err();
    assert!(matches!(error, Error::RotationIncomplete { .. }), "{error}");
    assert!(error.to_string().contains("may have changed backend state"));
    assert_eq!(
        fs::read_to_string(state.join("keys/cache.pub"))
            .unwrap()
            .trim(),
        new
    );
    assert!(state.join("celler-rotation.json").is_file());
    assert!(matches!(
        Publisher::load(&state, &workspace),
        Err(Error::PendingRotation)
    ));
    fs::remove_dir(obstruction).unwrap();
    assert_eq!(rotate().await.unwrap().as_str(), new);
    assert!(!state.join("celler-rotation.json").exists());
    server.finish();
}

#[tokio::test]
async fn missing_binding_conflict_is_recoverable_by_explicit_native_rotation_only() {
    let fixture = Fixture::new();
    let namespace = private_namespace(&fixture.workspace).unwrap();
    let client = fixture.server.client(namespace.as_str());
    let backend_key = client.register(RETENTION).await.unwrap();
    fs::create_dir_all(fixture.state.join("keys")).unwrap();
    let path = fixture.state.join("keys/cache.pub");
    fs::write(&path, super::PUBLIC_KEY).unwrap();
    assert!(matches!(fixture.provision().await,
        Err(Error::RegistrationCommitted { source }) if matches!(*source, Error::RetainedKeyConflict)));
    assert_eq!(fs::read_to_string(&path).unwrap(), super::PUBLIC_KEY);
    assert!(Publisher::load(&fixture.state, &fixture.workspace).is_err());
    let archive =
        native::archive::Archive::file("only selected view is cleared", "rotation-recovery");
    fixture.server.upload(namespace.as_str(), &archive).await;
    let common = "wrix-common";
    let peer = fixture.server.client(common);
    let peer_key = peer.register(RETENTION).await.unwrap();
    fixture.server.upload(common, &archive).await;
    let key = fixture.rotate().await.unwrap();
    assert_ne!(key, backend_key);
    assert_eq!(fs::read_to_string(&path).unwrap().trim(), key.as_str());
    assert_eq!(fixture.provision().await.unwrap(), key);
    assert_eq!(
        Publisher::load(&fixture.state, &fixture.workspace)
            .unwrap()
            .register()
            .await
            .unwrap(),
        key
    );
    assert_eq!(
        fixture
            .server
            .get(&format!(
                "{namespace}/{}.narinfo",
                native::hash(&archive.path)
            ))
            .await
            .status(),
        404
    );
    assert_eq!(peer.public_key().await.unwrap(), peer_key);
    assert_eq!(
        fixture
            .server
            .get(&format!("{common}/{}.narinfo", native::hash(&archive.path)))
            .await
            .status(),
        200
    );
    fixture.server.upload(namespace.as_str(), &archive).await;
    let imported = native::import::Store::new();
    let output = imported.copy(
        &format!("{}{namespace}", fixture.server.endpoint),
        &wrix_cache::backend::issuer::Authority::open(&fixture.server.issuer_root())
            .unwrap()
            .publication_token(&namespace)
            .unwrap(),
        key.as_str(),
        &archive.path,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn interrupted_native_rotation_finishes_recording_without_invalidating_replacement_content() {
    for recorded_key in [false, true] {
        let fixture = Fixture::new();
        let old = fixture.provision().await.unwrap();
        fixture.seed_intent(&old);
        let namespace = private_namespace(&fixture.workspace).unwrap();
        let key = fixture
            .server
            .client(namespace.as_str())
            .rotate(RETENTION)
            .await
            .unwrap();
        let archive =
            native::archive::Archive::file("replacement must survive retry", "rotation-retry");
        fixture.server.upload(namespace.as_str(), &archive).await;
        if recorded_key {
            fs::write(fixture.state.join("keys/cache.pub"), key.as_str()).unwrap();
        }
        assert!(matches!(
            fixture.provision().await,
            Err(Error::PendingRotation)
        ));
        assert!(matches!(
            Publisher::load(&fixture.state, &fixture.workspace),
            Err(Error::PendingRotation)
        ));
        assert_eq!(fixture.rotate().await.unwrap(), key);
        assert_eq!(fixture.provision().await.unwrap(), key);
        assert_eq!(fixture.provision().await.unwrap(), key);
        assert_eq!(
            fixture
                .server
                .get(&format!(
                    "{namespace}/{}.narinfo",
                    native::hash(&archive.path)
                ))
                .await
                .status(),
            200
        );
    }
}

#[tokio::test]
async fn native_rotation_rejects_wrong_workspace_binding_without_changing_backend_identity() {
    let fixture = Fixture::new();
    let key = fixture.provision().await.unwrap();
    let other = WorkspaceHash::parse(&"b".repeat(64)).unwrap();
    Publisher::install_issuer(
        &fixture.state,
        &other,
        &fixture.server.endpoint,
        private_namespace(&other).unwrap(),
        &fixture.server.issuer_root(),
        RETENTION,
    )
    .unwrap();
    assert!(matches!(fixture.rotate().await, Err(Error::Workspace)));
    assert_eq!(
        fixture
            .server
            .client(private_namespace(&fixture.workspace).unwrap().as_str())
            .public_key()
            .await
            .unwrap(),
        key
    );
}

#[tokio::test]
async fn native_rotation_rejects_mismatched_authority_coordinates_before_invalidation() {
    let fixture = Fixture::new();
    let key = fixture.provision().await.unwrap();
    let namespace = private_namespace(&fixture.workspace).unwrap();
    let other_issuer = fixture.root.path().join("other-issuer");
    let verification = fixture.root.path().join("verification");
    fs::create_dir(&verification).unwrap();
    wrix_cache::backend::issuer::Authority::provision(&other_issuer, &verification).unwrap();
    for (endpoint, name, issuer) in [
        (
            "http://127.0.0.1:1/",
            namespace.clone(),
            fixture.server.issuer_root(),
        ),
        (
            fixture.server.endpoint.as_str(),
            attic::cache::CacheName::new("peer".into()).unwrap(),
            fixture.server.issuer_root(),
        ),
        (
            fixture.server.endpoint.as_str(),
            namespace.clone(),
            other_issuer,
        ),
    ] {
        Publisher::install_issuer(
            &fixture.state,
            &fixture.workspace,
            endpoint,
            name,
            &issuer,
            RETENTION,
        )
        .unwrap();
        assert!(matches!(fixture.rotate().await, Err(Error::Binding)));
        assert_eq!(
            fixture
                .server
                .client(namespace.as_str())
                .public_key()
                .await
                .unwrap(),
            key
        );
    }
}

#[tokio::test]
async fn interrupted_rotation_resumes_before_deletion_or_after_confirmed_deletion() {
    for deleted in [false, true] {
        let fixture = Fixture::new();
        let old = fixture.provision().await.unwrap();
        fixture.seed_intent(&old);
        let namespace = private_namespace(&fixture.workspace).unwrap();
        if deleted {
            let token = wrix_cache::backend::issuer::Authority::open(&fixture.server.issuer_root())
                .unwrap()
                .publication_token(&namespace)
                .unwrap();
            let response = reqwest::Client::new()
                .delete(format!(
                    "{}_api/v1/cache-config/{namespace}",
                    fixture.server.endpoint
                ))
                .bearer_auth(token)
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success());
        }
        let new = fixture.rotate().await.unwrap();
        assert_ne!(old, new);
        assert_eq!(fixture.provision().await.unwrap(), new);
    }
}

#[tokio::test]
async fn native_rotation_requires_existing_issuer_even_when_binding_is_missing() {
    let fixture = Fixture::new();
    let key = fixture.provision().await.unwrap();
    fs::remove_file(fixture.state.join("celler-publisher.json")).unwrap();
    let missing = fixture.root.path().join("missing-issuer");
    assert!(
        rotate_private(
            &fixture.state,
            &fixture.workspace,
            &fixture.server.endpoint,
            &missing,
            RETENTION,
            std::time::Duration::ZERO
        )
        .await
        .is_err()
    );
    assert!(!missing.exists());
    assert_eq!(
        fixture
            .server
            .client(private_namespace(&fixture.workspace).unwrap().as_str())
            .public_key()
            .await
            .unwrap(),
        key
    );
}

#[tokio::test]
async fn native_rotation_rejects_unsafe_public_metadata_and_journal_before_invalidation() {
    let fixture = Fixture::new();
    let key = fixture.provision().await.unwrap();
    let path = fixture.state.join("keys/cache.pub");
    let target = fixture.root.path().join("preserved");
    fs::write(&target, key.as_str()).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&target, &path).unwrap();
    assert!(fixture.rotate().await.is_err());
    fs::remove_file(&path).unwrap();
    fs::hard_link(&target, &path).unwrap();
    assert!(matches!(
        fixture.rotate().await,
        Err(Error::UnsafeRegistrationFile)
    ));
    fs::remove_file(&path).unwrap();
    fs::write(&path, key.as_str()).unwrap();
    symlink(&target, fixture.state.join("celler-rotation.json")).unwrap();
    assert!(fixture.rotate().await.is_err());
    assert_eq!(
        fixture
            .server
            .client(private_namespace(&fixture.workspace).unwrap().as_str())
            .public_key()
            .await
            .unwrap(),
        key
    );
    assert_eq!(fs::read_to_string(target).unwrap(), key.as_str());
}
