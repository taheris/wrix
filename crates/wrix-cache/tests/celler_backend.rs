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

#[tokio::test]
async fn rotation_destroys_and_recreates_only_the_selected_namespace() {
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
        server.client().rotate(RETENTION).await.unwrap().as_str(),
        PUBLIC_KEY
    );
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
    let replacement = client.rotate(RETENTION).await.unwrap();
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
