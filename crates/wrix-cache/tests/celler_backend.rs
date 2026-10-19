#![cfg(test)]

use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::Duration,
};

use attic::cache::CacheName;
use futures::StreamExt as _;
use serde_json::{Value, json};
use wrix_cache::backend::{Client, Error};

#[path = "common/celler/mod.rs"]
mod native;

const PUBLIC_KEY: &str = "fixture:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const RETENTION: Duration = Duration::from_hours(720);

struct Exchange {
    method: &'static str,
    status: u16,
    response: Value,
    body: Option<Value>,
}

struct Server {
    endpoint: String,
    worker: thread::JoinHandle<()>,
}

impl Server {
    fn start(exchanges: Vec<Exchange>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            for exchange in exchanges {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < deadline, "missing HTTP request");
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("accept failed: {error}"),
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                    assert!(header.len() <= 8192);
                }
                let header = String::from_utf8(header).unwrap();
                assert_eq!(
                    header.lines().next().unwrap(),
                    format!("{} /_api/v1/cache-config/private HTTP/1.1", exchange.method)
                );
                let lower = header.to_ascii_lowercase();
                assert!(lower.contains("authorization: bearer fixture-publisher\r\n"));
                let length = lower
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .map_or(0, |value| value.parse::<usize>().unwrap());
                let mut body = vec![0; length];
                socket.read_exact(&mut body).unwrap();
                if let Some(expected) = exchange.body {
                    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), expected);
                } else {
                    assert_eq!(body, [] as [u8; 0]);
                }
                let response = serde_json::to_vec(&exchange.response).unwrap();
                write!(socket,
                    "HTTP/1.1 {} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    exchange.status, response.len()).unwrap();
                socket.write_all(&response).unwrap();
            }
        });
        Self { endpoint, worker }
    }

    fn client(&self) -> Client {
        Client::new(
            &self.endpoint,
            CacheName::new(String::from("private")).unwrap(),
            "fixture-publisher",
        )
        .unwrap()
    }

    fn finish(self) {
        self.worker.join().unwrap();
    }
}

fn config(key: &str) -> Value {
    json!({"public_key":key, "is_public":false, "retention_period":{"Period": RETENTION.as_secs()}})
}

const fn get(response: Value) -> Exchange {
    Exchange {
        method: "GET",
        status: 200,
        response,
        body: None,
    }
}

fn retention_patch() -> Exchange {
    Exchange {
        method: "PATCH",
        status: 200,
        response: Value::Null,
        body: Some(json!({"is_public":false,"retention_period":{"Period":RETENTION.as_secs()}})),
    }
}

#[tokio::test]
async fn registration_preserves_a_valid_existing_server_key() {
    let server = Server::start(vec![
        get(config(PUBLIC_KEY)),
        retention_patch(),
        get(config(PUBLIC_KEY)),
    ]);
    assert_eq!(
        server.client().register(RETENTION).await.unwrap().as_str(),
        PUBLIC_KEY
    );
    server.finish();
}

#[tokio::test]
async fn registration_requests_server_generation_without_receiving_private_keys() {
    let server = Server::start(vec![
        Exchange {
            method: "GET",
            status: 404,
            response: Value::Null,
            body: None,
        },
        Exchange {
            method: "POST",
            status: 200,
            response: Value::Null,
            body: Some(
                json!({"keypair":"Generate", "is_public":false, "store_dir":"/nix/store", "priority":40,"upstream_cache_key_names":[]}),
            ),
        },
        retention_patch(),
        get(config(PUBLIC_KEY)),
    ]);
    assert_eq!(
        server.client().register(RETENTION).await.unwrap().as_str(),
        PUBLIC_KEY
    );
    server.finish();
}

#[tokio::test]
async fn malformed_existing_key_uses_only_conditional_repair() {
    let server = Server::start(vec![
        get(config("invalid")),
        Exchange {
            method: "PATCH",
            status: 500,
            response: Value::Null,
            body: Some(json!({"keypair":"GenerateIfInvalid"})),
        },
    ]);
    assert!(matches!(
        server.client().register(RETENTION).await,
        Err(Error::Status(reqwest::StatusCode::INTERNAL_SERVER_ERROR))
    ));
    server.finish();
}

#[tokio::test]
async fn registration_requires_confirmed_active_retention() {
    let mut wrong = config(PUBLIC_KEY);
    wrong["retention_period"] = json!({"Period": 0});
    let server = Server::start(vec![get(config(PUBLIC_KEY)), retention_patch(), get(wrong)]);
    assert!(matches!(
        server.client().register(RETENTION).await,
        Err(Error::Protocol)
    ));
    server.finish();
}

#[tokio::test]
async fn failed_creation_reports_failure_without_a_key_patch() {
    let server = Server::start(vec![
        Exchange {
            method: "GET",
            status: 404,
            response: Value::Null,
            body: None,
        },
        Exchange {
            method: "POST",
            status: 500,
            response: json!({"message":"private-backend-detail"}),
            body: Some(
                json!({"keypair":"Generate", "is_public":false, "store_dir":"/nix/store", "priority":40,"upstream_cache_key_names":[]}),
            ),
        },
    ]);
    let error = server.client().register(RETENTION).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "Celler returned HTTP 500 Internal Server Error"
    );
    server.finish();
}

#[test]
fn http_fixture_uses_native_celler_configuration_schema() {
    let native: attic::api::v1::cache_config::CacheConfig =
        serde_json::from_value(config(PUBLIC_KEY)).unwrap();
    assert_eq!(native.public_key.as_deref(), Some(PUBLIC_KEY));
    assert_eq!(native.is_public, Some(false));
    assert!(matches!(native.retention_period,
        Some(attic::api::v1::cache_config::RetentionPeriodConfig::Period(seconds))
        if u64::from(seconds) == RETENTION.as_secs()));
}

fn rotation_exchanges() -> Vec<Exchange> {
    vec![
        Exchange {
            method: "DELETE",
            status: 200,
            response: Value::Null,
            body: None,
        },
        Exchange {
            method: "GET",
            status: 404,
            response: Value::Null,
            body: None,
        },
        Exchange {
            method: "POST",
            status: 200,
            response: Value::Null,
            body: Some(
                json!({"keypair":"Generate", "is_public":false, "store_dir":"/nix/store", "priority":40,"upstream_cache_key_names":[]}),
            ),
        },
        retention_patch(),
        get(config(PUBLIC_KEY)),
    ]
}

#[tokio::test]
async fn rotation_destroys_and_recreates_only_the_selected_namespace() {
    let server = Server::start(rotation_exchanges());
    assert_eq!(
        server.client().rotate(RETENTION).await.unwrap().as_str(),
        PUBLIC_KEY
    );
    server.finish();
}

#[tokio::test]
async fn failed_rotation_reports_invalidation_only_after_confirmed_deletion() {
    let server = Server::start(vec![Exchange {
        method: "DELETE",
        status: 403,
        response: Value::Null,
        body: None,
    }]);
    assert!(matches!(
        server.client().rotate(RETENTION).await,
        Err(Error::Status(reqwest::StatusCode::FORBIDDEN))
    ));
    server.finish();

    let server = Server::start(vec![
        Exchange {
            method: "DELETE",
            status: 200,
            response: Value::Null,
            body: None,
        },
        Exchange {
            method: "GET",
            status: 404,
            response: Value::Null,
            body: None,
        },
        Exchange {
            method: "POST",
            status: 403,
            response: Value::Null,
            body: Some(
                json!({"keypair":"Generate", "is_public":false, "store_dir":"/nix/store", "priority":40,"upstream_cache_key_names":[]}),
            ),
        },
    ]);
    let error = server.client().rotate(RETENTION).await.unwrap_err();
    assert!(matches!(
        error,
        Error::RotationInvalidated { ref source }
            if matches!(**source, Error::Status(reqwest::StatusCode::FORBIDDEN))
    ));
    assert!(error.to_string().contains("invalidated the selected view"));
    server.finish();
}

#[tokio::test]
async fn invalid_retention_is_rejected_before_backend_dispatch() {
    let client = Client::new(
        "http://127.0.0.1:1/",
        CacheName::new(String::from("private")).unwrap(),
        "fixture",
    )
    .unwrap();
    for duration in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_secs(i32::MAX as u64 + 1),
    ] {
        assert!(matches!(
            client.register(duration).await,
            Err(Error::Retention)
        ));
        assert!(matches!(
            client.rotate(duration).await,
            Err(Error::Retention)
        ));
    }
}

#[test]
fn publication_authority_rejects_nonlocal_origins_and_credential_injection() {
    for endpoint in [
        "file:///tmp/cache",
        "https://cache.example/",
        "http://localhost/",
        "http://127.0.0.1/other",
        "http://user@127.0.0.1/",
        "http://127.0.0.1/?token=secret",
    ] {
        assert!(matches!(
            Client::new(
                endpoint,
                CacheName::new(String::from("private")).unwrap(),
                "fixture"
            ),
            Err(Error::Endpoint)
        ));
    }
    for token in ["", "secret\r\nx-injected: true", "secret value"] {
        assert!(matches!(
            Client::new(
                "http://127.0.0.1/",
                CacheName::new(String::from("private")).unwrap(),
                token
            ),
            Err(Error::Credential)
        ));
    }
}

#[tokio::test]
async fn inconclusive_configuration_failure_allows_only_native_conditional_repair() {
    let server = Server::start(vec![
        Exchange {
            method: "GET",
            status: 500,
            response: Value::Null,
            body: None,
        },
        Exchange {
            method: "PATCH",
            status: 200,
            response: Value::Null,
            body: Some(json!({"keypair":"GenerateIfInvalid"})),
        },
        get(config(PUBLIC_KEY)),
        retention_patch(),
        get(config(PUBLIC_KEY)),
    ]);
    assert_eq!(
        server.client().register(RETENTION).await.unwrap().as_str(),
        PUBLIC_KEY
    );
    server.finish();
}

#[tokio::test]
async fn registration_rejects_unexpected_valid_key_changes() {
    let changed = "other:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    let server = Server::start(vec![
        get(config(PUBLIC_KEY)),
        retention_patch(),
        get(config(changed)),
    ]);
    assert!(matches!(
        server.client().register(RETENTION).await,
        Err(Error::KeyChanged)
    ));
    server.finish();
}

fn write_binding(
    root: &std::path::Path,
    workspace: &wrix_core::path::WorkspaceHash,
    endpoint: &str,
    token: &str,
) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    wrix_cache::backend::publication::Publisher::install(
        root,
        workspace,
        endpoint,
        CacheName::new(String::from("private")).unwrap(),
        token,
        RETENTION,
    )
    .unwrap();
}

#[tokio::test]
async fn authority_installation_is_private_atomic_and_loadable() {
    use std::os::unix::fs::MetadataExt as _;
    use wrix_cache::backend::publication::Publisher;
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("workspace-control");
    let workspace = wrix_core::path::Workspace::from_path(root.path()).unwrap();
    let server = Server::start(vec![
        get(config(PUBLIC_KEY)),
        retention_patch(),
        get(config(PUBLIC_KEY)),
    ]);
    Publisher::install(
        &state,
        workspace.hash(),
        "http://127.0.0.1:1/",
        CacheName::new(String::from("private")).unwrap(),
        "previous-token",
        RETENTION,
    )
    .unwrap();
    let path = state.join("celler-publisher.json");
    let mut old_file = std::fs::File::open(&path).unwrap();
    let old_inode = old_file.metadata().unwrap().ino();
    write_binding(
        &state,
        workspace.hash(),
        &server.endpoint,
        "fixture-publisher",
    );
    assert_ne!(std::fs::metadata(&path).unwrap().ino(), old_inode);
    let mut previous = String::new();
    old_file.read_to_string(&mut previous).unwrap();
    assert!(previous.contains("previous-token"));
    let current: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(current["token"], "fixture-publisher");
    assert_eq!(std::fs::metadata(&state).unwrap().mode() & 0o077, 0);
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(std::fs::read_dir(&state).unwrap().count(), 1);
    let publisher = Publisher::load(&state, workspace.hash()).unwrap();
    assert_eq!(publisher.register().await.unwrap().as_str(), PUBLIC_KEY);
    server.finish();
}

#[test]
fn authority_installation_validates_before_any_state_mutation() {
    use wrix_cache::backend::publication::Publisher;
    let root = tempfile::tempdir().unwrap();
    let workspace = wrix_core::path::Workspace::from_path(root.path()).unwrap();
    let oversized = "s".repeat(16 * 1024);
    for (endpoint, token, retention) in [
        ("file:///tmp/cache", "secret", RETENTION),
        ("http://127.0.0.1/", "secret with spaces", RETENTION),
        ("http://127.0.0.1/", "secret", Duration::ZERO),
        ("http://127.0.0.1/", "secret", Duration::from_nanos(1)),
        ("http://127.0.0.1/", oversized.as_str(), RETENTION),
    ] {
        let missing = root.path().join("not-created");
        let result = Publisher::install(
            &missing,
            workspace.hash(),
            endpoint,
            CacheName::new(String::from("private")).unwrap(),
            token,
            retention,
        );
        let error = result.unwrap_err();
        assert!(!format!("{error:?}; {error}").contains(token));
        assert!(!missing.exists());

        let existing = root.path().join("existing");
        std::fs::create_dir_all(&existing).unwrap();
        write_binding(&existing, workspace.hash(), "http://127.0.0.1/", "original");
        let path = existing.join("celler-publisher.json");
        let before = std::fs::read(&path).unwrap();
        assert!(
            Publisher::install(
                &existing,
                workspace.hash(),
                endpoint,
                CacheName::new(String::from("private")).unwrap(),
                token,
                retention,
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[test]
fn authority_installation_refuses_unsafe_targets_without_repairing_them() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use wrix_cache::backend::publication::Publisher;
    let root = tempfile::tempdir().unwrap();
    let workspace = wrix_core::path::Workspace::from_path(root.path()).unwrap();
    let state = root.path().join("control");
    std::fs::create_dir(&state).unwrap();
    let install = || {
        Publisher::install(
            &state,
            workspace.hash(),
            "http://127.0.0.1/",
            CacheName::new(String::from("private")).unwrap(),
            "fixture-publisher",
            RETENTION,
        )
    };
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(install().is_err());
    assert!(!state.join("celler-publisher.json").exists());
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = state.join("celler-publisher.json");
    let other = root.path().join("other");
    std::fs::write(&other, "do not replace").unwrap();
    symlink(&other, &path).unwrap();
    assert!(install().is_err());
    assert!(std::fs::symlink_metadata(&path).unwrap().is_symlink());
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&other, &path).unwrap();
    assert!(install().is_err());
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "do not replace");
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&state).unwrap();
    symlink(root.path(), &state).unwrap();
    assert!(install().is_err());
    assert!(!root.path().join("celler-publisher.json").exists());
}

#[tokio::test]
async fn runtime_authority_loads_only_the_bound_private_view() {
    use wrix_cache::backend::publication::Publisher;
    let root = tempfile::tempdir().unwrap();
    let workspace = wrix_core::path::Workspace::from_path(root.path()).unwrap();
    let server = Server::start(vec![
        get(config(PUBLIC_KEY)),
        retention_patch(),
        get(config(PUBLIC_KEY)),
    ]);
    write_binding(
        root.path(),
        workspace.hash(),
        &server.endpoint,
        "fixture-publisher",
    );
    let publisher = Publisher::load(root.path(), workspace.hash()).unwrap();
    assert_eq!(publisher.register().await.unwrap().as_str(), PUBLIC_KEY);
    server.finish();
}

#[test]
fn runtime_authority_rejects_unsafe_files_without_exposing_credentials() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use wrix_cache::backend::publication::{Error as BindingError, Publisher};
    let root = tempfile::tempdir().unwrap();
    let workspace = wrix_core::path::Workspace::from_path(root.path()).unwrap();
    let path = root.path().join("celler-publisher.json");
    let secret = "disposable-private-runtime-token";
    write_binding(root.path(), workspace.hash(), "http://127.0.0.1/", secret);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        Publisher::load(root.path(), workspace.hash()),
        Err(BindingError::Permissions)
    ));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        Publisher::load(root.path(), workspace.hash()),
        Err(BindingError::Permissions)
    ));
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let other = root.path().join("other.json");
    std::fs::rename(&path, &other).unwrap();
    symlink(&other, &path).unwrap();
    assert!(matches!(
        Publisher::load(root.path(), workspace.hash()),
        Err(BindingError::Io(_))
    ));
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&other, &path).unwrap();
    assert!(matches!(
        Publisher::load(root.path(), workspace.hash()),
        Err(BindingError::Permissions)
    ));
    std::fs::remove_file(other).unwrap();
    for body in [
        format!("{{\"token\":\"{secret}\"}}"),
        "x".repeat(16 * 1024 + 1),
    ] {
        std::fs::write(&path, body).unwrap();
        let Err(error) = Publisher::load(root.path(), workspace.hash()) else {
            panic!("unsafe binding accepted");
        };
        assert!(matches!(error, BindingError::Binding));
        assert!(!format!("{error:?}; {error}").contains(secret));
    }
}

#[test]
fn production_rotation_reports_backend_success_and_local_recording_failure() {
    let root = tempfile::tempdir().unwrap();
    let workspace = wrix_core::path::Workspace::from_path(root.path()).unwrap();
    let server = Server::start(rotation_exchanges());
    write_binding(
        root.path(),
        workspace.hash(),
        &server.endpoint,
        "fixture-publisher",
    );
    std::fs::write(root.path().join("keys"), "cannot be a directory").unwrap();
    let error = wrix_cache::publisher::run_workspace_at(
        &workspace,
        root.path(),
        &root.path().join("unused-archive-root"),
        wrix_cache::publisher::Mode::RotateKey,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        wrix_cache::publisher::Error::RotationRecording { .. }
    ));
    assert!(error.to_string().contains("Celler view was rotated"));
    assert!(!root.path().join("cache-status.json").exists());
    server.finish();
}

#[test]
fn runtime_authority_rejects_scope_drift_and_invalid_native_policy() {
    use wrix_cache::backend::publication::{Error as BindingError, Publisher};
    let root = tempfile::tempdir().unwrap();
    let peer = tempfile::tempdir().unwrap();
    let workspace = wrix_core::path::Workspace::from_path(root.path()).unwrap();
    let peer = wrix_core::path::Workspace::from_path(peer.path()).unwrap();
    write_binding(
        root.path(),
        workspace.hash(),
        "http://127.0.0.1/",
        "fixture-publisher",
    );
    assert!(matches!(
        Publisher::load(root.path(), peer.hash()),
        Err(BindingError::Workspace)
    ));
    let path = root.path().join("celler-publisher.json");
    let binding: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for (field, value) in [
        ("schema_version", json!(2)),
        ("retention_seconds", json!(0)),
        ("retention_seconds", json!(u64::MAX)),
        ("workspace_hash", json!("invalid")),
        ("endpoint", json!("file:///tmp/cache")),
        ("namespace", json!("../peer")),
        ("public", json!(true)),
    ] {
        let mut invalid = binding.clone();
        invalid[field] = value;
        std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            Publisher::load(root.path(), workspace.hash()).is_err(),
            "accepted {field}"
        );
    }
}

#[tokio::test]
#[ignore = "requires packaged Celler, Linux user/mount namespaces, Nix and OpenSSL"]
async fn publisher_reuses_rust_store_client() {
    if !native::in_isolated_store("publisher_reuses_rust_store_client") {
        return;
    }
    let path = native::add_file("native Rust store transport", "native-store");
    let store = Box::pin(attic::nix_store::NixStore::connect())
        .await
        .unwrap();
    let parsed = store.parse_store_path(&path).unwrap();
    let info = store.query_path_info(&parsed).await.unwrap().unwrap();
    let mut nar = store.nar_from_path(parsed);
    let mut bytes = Vec::new();
    while let Some(chunk) = nar.next().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(u64::try_from(bytes.len()).unwrap(), info.nar_size);
    let server = native::Server::start();
    let client = server.client("private");
    client.register(RETENTION).await.unwrap();
    assert_eq!(
        client
            .publish_selected(&[path.clone(), path.clone()])
            .await
            .unwrap(),
        1
    );
    let narinfo = server
        .get(&format!("private/{}.narinfo", native::hash(&path)))
        .await;
    assert_eq!(narinfo.status(), 200);
    let narinfo = narinfo.text().await.unwrap();
    let url = narinfo
        .lines()
        .find_map(|line| line.strip_prefix("URL: "))
        .unwrap();
    let received = server
        .get(&format!("private/{url}"))
        .await
        .bytes()
        .await
        .unwrap();
    assert_eq!(received.as_ref(), bytes);
}

#[tokio::test]
#[ignore = "requires packaged Celler, Linux user/mount namespaces, Nix and OpenSSL"]
async fn managed_layout_preserves_signing_and_archives_across_native_backend_replacement() {
    if !native::in_isolated_store(
        "managed_layout_preserves_signing_and_archives_across_native_backend_replacement",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let layout = wrix_cache::backend::layout::Layout::resolve(
        wrix_cache::backend::layout::Platform::Linux,
        root.path(),
        None,
        None,
    )
    .unwrap();
    let path = native::add_file("persistent native Celler archive", "persistent-celler");
    let server = native::Server::start_in_layout(&layout);
    let client = server.client("private");
    let key = client.register(RETENTION).await.unwrap();
    assert_eq!(
        client
            .publish_selected(std::slice::from_ref(&path))
            .await
            .unwrap(),
        1
    );
    let resource = format!("private/{}.narinfo", native::hash(&path));
    let before = server.get(&resource).await.text().await.unwrap();
    assert!(before.contains("Sig: "));
    let url = before
        .lines()
        .find_map(|line| line.strip_prefix("URL: "))
        .unwrap();
    let archive = server
        .get(&format!("private/{url}"))
        .await
        .bytes()
        .await
        .unwrap();
    drop(client);
    drop(server);
    let replacement = native::Server::start_in_layout(&layout);
    assert_eq!(
        replacement
            .client("private")
            .register(RETENTION)
            .await
            .unwrap(),
        key
    );
    let response = replacement.get(&resource).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), before);
    let response = replacement.get(&format!("private/{url}")).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap(), archive);
    assert!(layout.metadata().join("metadata.db").is_file());
    for mount in layout.mounts() {
        assert!(!mount.source.join("celler-publisher.json").exists());
        assert!(!mount.source.join("publish-roots.json").exists());
        assert!(!mount.source.join("jwt.key").exists());
    }
}

async fn invoke_native_hook(
    control: &std::path::Path,
    selected: &std::path::Path,
) -> wrix_cache::publisher::Report {
    let control = control.to_path_buf();
    let selected = selected.to_str().unwrap().to_owned();
    tokio::task::spawn_blocking(move || {
        let workspace = wrix_core::path::Workspace::from_path(&control).unwrap();
        wrix_cache::publisher::run_hook_record(
            wrix_cache::publisher::HookRecord {
                workspace_hash: workspace.hash(),
                state_root: &control,
                cache_root: &control.join("legacy-cache"),
                manifest: &control.join("publish-roots.json"),
                drv_path: "selected.drv",
                out_paths: &selected,
            },
            wrix_cache::config::Settings::default(),
        )
    })
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
#[ignore = "requires packaged Celler, Linux user/mount namespaces, Nix and OpenSSL"]
async fn native_hook_upload_failure_records_pending_without_flat_fallback() {
    if !native::in_isolated_store(
        "native_hook_upload_failure_records_pending_without_flat_fallback",
    ) {
        return;
    }
    let path = native::add_file("denied native upload", "denied-hook-output");
    let server = native::Server::start();
    let client = server.client("private");
    let key = client.register(RETENTION).await.unwrap();
    let control = tempfile::tempdir().unwrap();
    let workspace = wrix_core::path::Workspace::from_path(control.path()).unwrap();
    let pull_token = native::pull_token();
    write_binding(
        control.path(),
        workspace.hash(),
        &server.endpoint,
        &pull_token,
    );
    std::fs::write(
        control.path().join("publish-roots.json"),
        json!({"roots": [{
            "name": "packages.demo", "installable": ".#demo", "drv_path": "selected.drv",
            "out_paths": [path]
        }]})
        .to_string(),
    )
    .unwrap();
    let report = invoke_native_hook(control.path(), &path).await;
    assert!(
        report
            .lines()
            .iter()
            .any(|line| line.contains("automatic project cache publish failed"))
    );
    let pending = std::fs::read_dir(control.path().join("pending"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let record: Value = serde_json::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
    assert_eq!(record["drv_path"], "selected.drv");
    assert_eq!(record["out_paths"], json!([path]));
    let status: Value =
        serde_json::from_slice(&std::fs::read(control.path().join("cache-status.json")).unwrap())
            .unwrap();
    assert_eq!(status["last_publish"], "warning");
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .contains("Celler returned HTTP")
    );
    assert!(!status.to_string().contains(&pull_token));
    assert!(!control.path().join("legacy-cache").exists());
    assert!(!control.path().join("keys").exists());
    assert_eq!(client.public_key().await.unwrap(), key);
    assert_eq!(
        server
            .get(&format!("private/{}.narinfo", native::hash(&path)))
            .await
            .status(),
        404
    );
}

#[tokio::test]
#[ignore = "requires packaged Celler, Linux user/mount namespaces, Nix and OpenSSL"]
async fn admitted_hook_streams_native_closure_without_flat_state() {
    if !native::in_isolated_store("admitted_hook_streams_native_closure_without_flat_state") {
        return;
    }
    let dependency = native::add_file("selected dependency", "hook-dependency");
    let unrelated = native::add_file("not selected", "hook-unrelated");
    let output = std::process::Command::new("nix")
        .args(["--store", "daemon", "eval", "--raw", "--impure", "--expr"])
        .arg(format!(
            "builtins.toFile \"hook-selected\" (builtins.storePath {:?})",
            dependency.to_str().unwrap()
        ))
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let selected = String::from_utf8(output.stdout).unwrap();
    let selected = std::path::PathBuf::from(selected.trim());
    let server = native::Server::start();
    let client = server.client("private");
    let key = client.register(RETENTION).await.unwrap();
    let control = tempfile::tempdir().unwrap();
    let workspace = wrix_core::path::Workspace::from_path(control.path()).unwrap();
    write_binding(
        control.path(),
        workspace.hash(),
        &server.endpoint,
        &server.token,
    );
    let manifest = control.path().join("publish-roots.json");
    std::fs::write(
        &manifest,
        json!({"roots": [{
            "name": "packages.demo", "installable": ".#demo", "drv_path": "selected.drv",
            "out_paths": [selected]
        }]})
        .to_string(),
    )
    .unwrap();
    let report = invoke_native_hook(control.path(), &selected).await;
    assert_eq!(
        report.lines(),
        ["published 2 project cache paths through Celler"]
    );
    assert!(!control.path().join("legacy-cache").exists());
    assert!(!control.path().join("keys").exists());
    assert!(!control.path().join("gcroots").exists());
    assert_eq!(
        std::fs::read_dir(control.path().join("pending"))
            .unwrap()
            .count(),
        0
    );
    for path in [&selected, &dependency] {
        let response = server
            .get(&format!("private/{}.narinfo", native::hash(path)))
            .await;
        assert_eq!(response.status(), 200);
        assert!(response.text().await.unwrap().contains(&format!(
            "Sig: {}:",
            key.as_str().split_once(':').unwrap().0
        )));
    }
    assert_eq!(
        server
            .get(&format!("private/{}.narinfo", native::hash(&unrelated)))
            .await
            .status(),
        404
    );
    assert_eq!(client.public_key().await.unwrap(), key);
}

#[tokio::test]
#[ignore = "requires packaged Celler, Linux user/mount namespaces, Nix and OpenSSL"]
async fn publication_uses_namespace_server_signing() {
    if !native::in_isolated_store("publication_uses_namespace_server_signing") {
        return;
    }
    let output = std::process::Command::new("nix-build")
        .args(["--store", "daemon", "--no-out-link", "--expr"])
        .arg("derivation { name = \"signed-native-store\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"echo signed > $out\" ]; }")
        .output().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let path = std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let store = Box::pin(attic::nix_store::NixStore::connect())
        .await
        .unwrap();
    let info = store
        .query_path_info(store.parse_store_path(&path).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(
        info.ca.is_none(),
        "signature control must be input-addressed"
    );
    let server = native::Server::start();
    let client = server.client("private");
    let key = client.register(RETENTION).await.unwrap();
    client
        .publish_selected(std::slice::from_ref(&path))
        .await
        .unwrap();
    let narinfo = server
        .get(&format!("private/{}.narinfo", native::hash(&path)))
        .await;
    assert_eq!(narinfo.status(), 200);
    let narinfo = narinfo.text().await.unwrap();
    let field = |name: &str| {
        narinfo
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap()
    };
    assert_eq!(field("StorePath: "), path.to_str().unwrap());
    let references = field("References: ")
        .split_whitespace()
        .map(|reference| format!("/nix/store/{reference}"))
        .collect::<Vec<_>>()
        .join(",");
    let fingerprint = format!(
        "1;{};{};{};{references}",
        field("StorePath: "),
        attic::hash::Hash::from_typed(field("NarHash: "))
            .unwrap()
            .to_typed_base32(),
        field("NarSize: ")
    );
    let public: attic::signing::NixPublicKey = key.as_str().parse().unwrap();
    public
        .verify(fingerprint.as_bytes(), field("Sig: "))
        .unwrap_or_else(|error| panic!("{error}: {fingerprint:?}; {narinfo}"));
    assert_eq!(client.public_key().await.unwrap(), key);
    assert_eq!(
        server
            .get(&format!("peer/{}.narinfo", native::hash(&path)))
            .await
            .status(),
        404
    );
}

#[tokio::test]
#[ignore = "requires packaged Celler, Linux user/mount namespaces, Nix and OpenSSL"]
async fn publication_is_scoped_without_recursive_expansion() {
    if !native::in_isolated_store("publication_is_scoped_without_recursive_expansion") {
        return;
    }
    let dependency = native::add_file("not admitted", "unselected-dependency");
    let output = std::process::Command::new("nix")
        .args(["--store", "daemon", "eval", "--raw", "--impure", "--expr"])
        .arg(format!(
            "builtins.toFile \"selected\" (builtins.storePath {:?})",
            dependency.to_str().unwrap()
        ))
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let path = std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let store = Box::pin(attic::nix_store::NixStore::connect())
        .await
        .unwrap();
    let info = store
        .query_path_info(store.parse_store_path(&path).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.references.len(), 1);
    let server = native::Server::start();
    let client = server.client("private");
    client.register(RETENTION).await.unwrap();
    assert_eq!(
        client
            .publish_selected(std::slice::from_ref(&path))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        server
            .get(&format!("private/{}.narinfo", native::hash(&path)))
            .await
            .status(),
        200
    );
    assert_eq!(
        server
            .get(&format!("private/{}.narinfo", native::hash(&dependency)))
            .await
            .status(),
        404
    );
}

#[tokio::test]
#[ignore = "requires packaged Celler, Linux user/mount namespaces, Nix and OpenSSL"]
async fn key_registration_and_failed_repair_preserve_valid_state() {
    if !native::in_isolated_store("key_registration_and_failed_repair_preserve_valid_state") {
        return;
    }
    let server = native::Server::start();
    let client = server.client("private");
    let peer = server.client("peer");
    let key = client.register(RETENTION).await.unwrap();
    let peer_key = peer.register(RETENTION).await.unwrap();
    assert_eq!(client.register(RETENTION).await.unwrap(), key);
    server.corrupt_key("private");
    assert!(server.pull_client().register(RETENTION).await.is_err());
    assert_eq!(peer.public_key().await.unwrap(), peer_key);
    assert!(matches!(
        client.public_key().await,
        Err(Error::Status(reqwest::StatusCode::INTERNAL_SERVER_ERROR))
    ));
    let repaired = client.register(RETENTION).await.unwrap();
    assert_ne!(repaired, key);
    assert_eq!(peer.public_key().await.unwrap(), peer_key);
    assert_eq!(client.register(RETENTION).await.unwrap(), repaired);
}

#[tokio::test]
#[ignore = "requires packaged Celler, Linux user/mount namespaces, Nix and OpenSSL"]
async fn rotation_invalidates_only_the_selected_view() {
    if !native::in_isolated_store("rotation_invalidates_only_the_selected_view") {
        return;
    }
    let path = native::add_file("retained by peer", "shared-native-store");
    let server = native::Server::start();
    let client = server.client("private");
    let peer = server.client("peer");
    let key = client.register(RETENTION).await.unwrap();
    let peer_key = peer.register(RETENTION).await.unwrap();
    client
        .publish_selected(std::slice::from_ref(&path))
        .await
        .unwrap();
    peer.publish_selected(std::slice::from_ref(&path))
        .await
        .unwrap();
    let control = tempfile::tempdir().unwrap();
    let workspace = wrix_core::path::Workspace::from_path(control.path()).unwrap();
    write_binding(
        control.path(),
        workspace.hash(),
        &server.endpoint,
        &server.token,
    );
    let control_path = control.path().to_path_buf();
    let legacy = control.path().join("legacy-cache");
    std::fs::create_dir(&legacy).unwrap();
    std::fs::write(legacy.join("keep"), "not a native Celler archive").unwrap();
    std::fs::write(
        control.path().join("cache-status.json"),
        serde_json::to_vec(&json!({
            "dirty": true, "last_publish": "ok", "last_error": null,
            "last_prune": "historic-only", "last_prune_epoch": 1
        }))
        .unwrap(),
    )
    .unwrap();
    let report = tokio::task::spawn_blocking(move || {
        wrix_cache::publisher::run_workspace_at(
            &workspace,
            &control_path,
            &legacy,
            wrix_cache::publisher::Mode::RotateKey,
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(report.lines().iter().any(|line| line.contains("republish")));
    let status: Value =
        serde_json::from_slice(&std::fs::read(control.path().join("cache-status.json")).unwrap())
            .unwrap();
    assert_eq!(status["last_prune"], "historic-only");
    assert_eq!(status["last_prune_epoch"], 1);
    let replacement = client.public_key().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(control.path().join("keys/cache.pub"))
            .unwrap()
            .trim(),
        replacement.as_str()
    );
    assert!(!control.path().join("keys/cache.secret").exists());
    assert_eq!(
        std::fs::read_to_string(control.path().join("legacy-cache/keep")).unwrap(),
        "not a native Celler archive"
    );
    assert_ne!(replacement, key);
    assert_eq!(client.public_key().await.unwrap(), replacement);
    assert_eq!(peer.public_key().await.unwrap(), peer_key);
    assert_eq!(
        server
            .get(&format!("private/{}.narinfo", native::hash(&path)))
            .await
            .status(),
        404
    );
    assert_eq!(
        server
            .get(&format!("peer/{}.narinfo", native::hash(&path)))
            .await
            .status(),
        200
    );
    client
        .publish_selected(std::slice::from_ref(&path))
        .await
        .unwrap();
    assert_eq!(client.public_key().await.unwrap(), replacement);
    assert_eq!(
        server
            .get(&format!("private/{}.narinfo", native::hash(&path)))
            .await
            .status(),
        200
    );
}
