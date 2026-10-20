use std::{fs, path::PathBuf};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use wrix_cache::backend::{
    issuer::Authority,
    publication::{
        Error as PublicationError, Publisher,
        registration::{private_namespace, provision_private},
    },
};
use wrix_core::path::Workspace;

use super::*;

struct Fixture {
    root: tempfile::TempDir,
    workspace: Workspace,
    issuer: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let metadata = root.path().join("metadata");
        fs::create_dir(&metadata).unwrap();
        let issuer = root.path().join("issuer");
        Authority::provision(&issuer, &metadata).unwrap();
        let workspace = Workspace::from_path(root.path()).unwrap();
        let state = root.path().join("control");
        Self {
            root,
            workspace,
            issuer,
            state,
        }
    }

    fn server(&self, exchanges: Vec<Exchange>) -> Server {
        let namespace = private_namespace(self.workspace.hash())
            .unwrap()
            .as_str()
            .to_owned();
        let expected = namespace.clone();
        Server::start_for_namespace(namespace, exchanges, move |header| {
            let token = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(": ")?;
                    name.eq_ignore_ascii_case("authorization").then_some(value)
                })
                .unwrap()
                .strip_prefix("Bearer ")
                .unwrap();
            let claims: Value = serde_json::from_slice(
                &URL_SAFE_NO_PAD
                    .decode(token.split('.').nth(1).unwrap())
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                claims["https://jwt.attic.rs/v1"]["caches"],
                json!({
                    (expected.as_str()): {"r":1,"w":1,"cc":1,"cr":1,"cq":1,"cd":1}
                })
            );
        })
    }

    async fn provision(
        &self,
        endpoint: &str,
    ) -> Result<wrix_core::cache_key::CachePublicKey, PublicationError> {
        provision_private(
            &self.state,
            self.workspace.hash(),
            endpoint,
            &self.issuer,
            RETENTION,
            Duration::ZERO,
        )
        .await
    }
}

#[tokio::test]
async fn registration_installs_only_confirmed_canonical_private_authority_and_key() {
    let fixture = Fixture::new();
    let server = fixture.server(vec![
        get(config(PUBLIC_KEY)),
        retention_patch(),
        get(config(PUBLIC_KEY)),
        get(config(PUBLIC_KEY)),
        retention_patch(),
        get(config(PUBLIC_KEY)),
        get(config(PUBLIC_KEY)),
        retention_patch(),
        get(config(PUBLIC_KEY)),
    ]);
    let peer = fixture.root.path().join("peer");
    fs::create_dir(&peer).unwrap();
    fs::write(peer.join("cache.pub"), "unchanged peer key").unwrap();
    let key = fixture.provision(&server.endpoint).await.unwrap();
    assert_eq!(key.as_str(), PUBLIC_KEY);
    assert_eq!(fixture.provision(&server.endpoint).await.unwrap(), key);
    let binding: Value =
        serde_json::from_slice(&fs::read(fixture.state.join("celler-publisher.json")).unwrap())
            .unwrap();
    assert_eq!(
        binding["namespace"],
        private_namespace(fixture.workspace.hash())
            .unwrap()
            .as_str()
    );
    assert_eq!(binding["issuer_root"], fixture.issuer.to_str().unwrap());
    assert!(binding.get("token").is_none());
    assert_eq!(
        fs::read_to_string(fixture.state.join("keys/cache.pub")).unwrap(),
        format!("{PUBLIC_KEY}\n")
    );
    assert_eq!(
        fs::read_to_string(peer.join("cache.pub")).unwrap(),
        "unchanged peer key"
    );
    let publisher = Publisher::load(&fixture.state, fixture.workspace.hash()).unwrap();
    assert_eq!(publisher.register().await.unwrap(), key);
    server.finish();
}

#[tokio::test]
async fn failed_authentication_does_not_install_publication_admission_or_key() {
    let fixture = Fixture::new();
    let server = fixture.server(vec![Exchange {
        method: "GET",
        status: 403,
        response: json!({"detail":"sensitive-backend-message"}),
        body: None,
    }]);
    let error = fixture.provision(&server.endpoint).await.unwrap_err();
    assert!(matches!(
        error,
        PublicationError::Backend(Error::Status(reqwest::StatusCode::FORBIDDEN))
    ));
    assert!(!format!("{error}; {error:?}").contains("sensitive-backend-message"));
    assert!(!fixture.state.join("celler-publisher.json").exists());
    assert!(!fixture.state.join("keys").exists());
    server.finish();
}

#[tokio::test]
async fn unconfirmed_registration_preserves_existing_local_admission() {
    let fixture = Fixture::new();
    let ready = fixture.server(vec![
        get(config(PUBLIC_KEY)),
        retention_patch(),
        get(config(PUBLIC_KEY)),
    ]);
    fixture.provision(&ready.endpoint).await.unwrap();
    ready.finish();
    let path = fixture.state.join("celler-publisher.json");
    let previous_binding = fs::read(&path).unwrap();
    let key = fixture.state.join("keys/cache.pub");
    let previous_key = fs::read(&key).unwrap();
    let mut unconfirmed = config(PUBLIC_KEY);
    unconfirmed["is_public"] = json!(true);
    let failed = fixture.server(vec![
        get(config(PUBLIC_KEY)),
        retention_patch(),
        get(unconfirmed),
    ]);
    assert!(matches!(
        fixture.provision(&failed.endpoint).await,
        Err(PublicationError::Backend(Error::Protocol))
    ));
    assert_eq!(fs::read(path).unwrap(), previous_binding);
    assert_eq!(fs::read(key).unwrap(), previous_key);
    failed.finish();
}

#[tokio::test]
async fn local_recording_failure_reports_active_backend_without_installing_a_publisher() {
    let fixture = Fixture::new();
    wrix_core::private_directory::prepare_with_parents(&fixture.state).unwrap();
    fs::write(fixture.state.join("keys"), "not a directory").unwrap();
    let server = fixture.server(vec![
        get(config(PUBLIC_KEY)),
        retention_patch(),
        get(config(PUBLIC_KEY)),
    ]);
    let error = fixture.provision(&server.endpoint).await.unwrap_err();
    assert!(matches!(
        error,
        PublicationError::RegistrationCommitted { .. }
    ));
    assert!(error.to_string().contains("backend changes remain active"));
    assert!(!fixture.state.join("celler-publisher.json").exists());
    assert_eq!(
        fs::read_to_string(fixture.state.join("keys")).unwrap(),
        "not a directory"
    );
    server.finish();
}

#[tokio::test]
async fn lock_contention_dispatches_no_registration_or_local_admission_changes() {
    use fs2::FileExt as _;
    use std::{fs::OpenOptions, os::unix::fs::PermissionsExt as _};
    let fixture = Fixture::new();
    wrix_core::private_directory::prepare_with_parents(&fixture.state).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(fixture.state.join("cache.lock"))
        .unwrap();
    fs::set_permissions(
        fixture.state.join("cache.lock"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    lock.lock_exclusive().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    assert!(matches!(
        fixture.provision(&endpoint).await,
        Err(PublicationError::LockTimeout)
    ));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    assert!(!fixture.state.join("celler-publisher.json").exists());
    assert!(!fixture.state.join("keys").exists());
}

#[tokio::test]
async fn invalid_retention_is_rejected_before_registration_state_or_dispatch() {
    let fixture = Fixture::new();
    let result = provision_private(
        &fixture.state,
        fixture.workspace.hash(),
        "http://127.0.0.1:1/",
        &fixture.issuer,
        Duration::ZERO,
        Duration::ZERO,
    )
    .await;
    assert!(matches!(
        result,
        Err(PublicationError::Backend(Error::Retention))
    ));
    assert!(!fixture.state.exists());
}
