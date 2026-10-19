#![cfg(test)]

use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::Duration,
};

use attic::cache::CacheName;
use serde_json::{Value, json};
use wrix_cache::backend::{Client, Error};

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
async fn malformed_existing_key_does_not_trigger_replacement() {
    let server = Server::start(vec![get(config("invalid"))]);
    assert!(matches!(
        server.client().register(RETENTION).await,
        Err(Error::PublicKey)
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
