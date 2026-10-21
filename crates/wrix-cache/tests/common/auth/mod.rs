use std::{
    io::Write,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Method, StatusCode};
use serde_json::json;
use wrix_cache::backend::{issuer::Authority, pull::Credential};
use wrix_core::path::WorkspaceHash;

use super::{RETENTION, native};

struct Fixture {
    server: native::Server,
    credential: Credential,
    peer: Credential,
}

impl Fixture {
    async fn new() -> Self {
        let server = native::Server::start_with_issuer();
        let authority = Authority::open(&server.issuer_root()).unwrap();
        let credential = authority
            .pull_credential(&WorkspaceHash::parse(&"a".repeat(64)).unwrap())
            .unwrap();
        let peer = authority
            .pull_credential(&WorkspaceHash::parse(&"b".repeat(64)).unwrap())
            .unwrap();
        for namespace in [
            credential.views().common(),
            credential.views().private(),
            peer.views().private(),
        ] {
            server
                .client(namespace.as_str())
                .register(RETENTION)
                .await
                .unwrap();
        }
        Self {
            server,
            credential,
            peer,
        }
    }

    async fn get(&self, resource: &str, token: Option<&str>) -> reqwest::Response {
        let request = reqwest::Client::new().get(format!("{}{resource}", self.server.endpoint));
        let request = match token {
            Some(token) => request.bearer_auth(token),
            None => request,
        };
        request.send().await.unwrap()
    }
}

#[tokio::test]
async fn native_pull_grants_read_authenticated_common_and_own_private_only() {
    let fixture = Fixture::new().await;
    let token = fixture.credential.expose_bearer();
    for namespace in [
        fixture.credential.views().common(),
        fixture.credential.views().private(),
    ] {
        let resource = format!("{namespace}/nix-cache-info");
        assert_eq!(
            fixture.get(&resource, Some(token)).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            fixture.get(&resource, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let peer = fixture.peer.views().private();
    let archive = native::archive::Archive::file("private peer content", "peer-only");
    fixture.server.upload(peer.as_str(), &archive).await;
    let resource = format!("{peer}/{}.narinfo", native::hash(&archive.path));
    let response = fixture
        .get(&resource, Some(fixture.peer.expose_bearer()))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let info = response.text().await.unwrap();
    let nar = info
        .lines()
        .find_map(|line| line.strip_prefix("URL: "))
        .unwrap();
    for resource in [resource, format!("{peer}/{nar}")] {
        assert_eq!(
            fixture
                .get(&resource, Some(fixture.peer.expose_bearer()))
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            fixture.get(&resource, Some(token)).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn native_readable_cache_metadata_omits_private_signing_material() {
    let fixture = Fixture::new().await;
    let namespace = fixture.credential.views().private();
    let response = fixture
        .get(
            &format!("_api/v1/cache-config/{namespace}"),
            Some(fixture.credential.expose_bearer()),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let config: attic::api::v1::cache_config::CacheConfig = response.json().await.unwrap();
    assert!(config.keypair.is_none());
    assert_eq!(
        config.public_key.as_deref(),
        Some(
            fixture
                .server
                .client(namespace.as_str())
                .public_key()
                .await
                .unwrap()
                .as_str()
        )
    );
}

#[tokio::test]
async fn native_pull_grants_cannot_manage_or_upload_even_to_their_own_view() {
    let fixture = Fixture::new().await;
    let private = fixture.credential.views().private();
    let route = format!("{}_api/v1/cache-config/{private}", fixture.server.endpoint);
    for method in [Method::POST, Method::PATCH, Method::DELETE] {
        let body = if method == Method::POST {
            serde_json::to_value(attic::api::v1::cache_config::CreateCacheRequest {
                keypair: attic::api::v1::cache_config::KeypairConfig::Generate,
                is_public: true,
                store_dir: String::from("/nix/store"),
                priority: 40,
                upstream_cache_key_names: Vec::new(),
            })
            .unwrap()
        } else {
            json!({"is_public": true})
        };
        let response = reqwest::Client::new()
            .request(method, &route)
            .bearer_auth(fixture.credential.expose_bearer())
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let archive =
        native::archive::Archive::file("forbidden worker upload", "no-worker-publication");
    let preamble = serde_json::to_vec(&archive.metadata(private.as_str())).unwrap();
    let mut body = preamble.clone();
    body.extend_from_slice(&archive.bytes);
    let response = reqwest::Client::new()
        .put(format!("{}_api/v1/upload-path", fixture.server.endpoint))
        .bearer_auth(fixture.credential.expose_bearer())
        .header(
            attic::api::v1::upload_path::CELLER_NAR_INFO_PREAMBLE_SIZE,
            preamble.len(),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        fixture
            .get(
                &format!("{private}/nix-cache-info"),
                Some(fixture.credential.expose_bearer())
            )
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture
            .get(
                &format!("{private}/{}.narinfo", native::hash(&archive.path)),
                Some(fixture.credential.expose_bearer())
            )
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn restricted_native_grants_do_not_retain_a_withdrawn_audience() {
    use wrix_cache::backend::pull::Audience;
    let fixture = Fixture::new().await;
    let authority = Authority::open(&fixture.server.issuer_root()).unwrap();
    let workspace = WorkspaceHash::parse(&"a".repeat(64)).unwrap();
    for audience in [Audience::Common, Audience::Private] {
        let renewed = authority
            .restricted_pull_credential(&workspace, audience)
            .unwrap();
        for (namespace, allowed) in [
            (renewed.views().common(), audience.common()),
            (renewed.views().private(), audience.private()),
        ] {
            let resource = format!("{namespace}/nix-cache-info");
            assert_eq!(
                fixture
                    .get(&resource, Some(renewed.expose_bearer()))
                    .await
                    .status(),
                if allowed {
                    StatusCode::OK
                } else {
                    StatusCode::UNAUTHORIZED
                }
            );
            assert_eq!(
                fixture
                    .get(&resource, Some(fixture.credential.expose_bearer()))
                    .await
                    .status(),
                StatusCode::OK
            );
        }
    }
}

#[tokio::test]
async fn real_rsa_verifier_rejects_expired_and_tampered_pull_grants() {
    let fixture = Fixture::new().await;
    let token = fixture.credential.expose_bearer();
    let parts: Vec<_> = token.split('.').collect();
    let mut claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    claims["iat"] = json!(now - 7200);
    claims["exp"] = json!(now - 3600);
    let message = format!(
        "{}.{}",
        parts[0],
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let mut signer = Command::new("openssl")
        .args(["dgst", "-sha256", "-sign"])
        .arg(fixture.server.issuer_root().join("issuer.pem"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    signer
        .stdin
        .take()
        .unwrap()
        .write_all(message.as_bytes())
        .unwrap();
    let output = signer.wait_with_output().unwrap();
    assert!(output.status.success());
    let expired = format!("{message}.{}", URL_SAFE_NO_PAD.encode(output.stdout));
    let tampered = format!("{message}.{}", parts[2]);
    let resource = format!("{}/nix-cache-info", fixture.credential.views().common());
    assert_eq!(
        fixture.get(&resource, Some(token)).await.status(),
        StatusCode::OK
    );
    for token in [&expired, &tampered] {
        assert_eq!(
            fixture.get(&resource, Some(token)).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn native_nix_imports_new_publications_with_an_existing_exact_pull_grant() {
    let fixture = Fixture::new().await;
    for namespace in [
        fixture.credential.views().common(),
        fixture.credential.views().private(),
    ] {
        let archive =
            native::archive::Archive::file("published after issuance", namespace.as_str());
        let key = fixture
            .server
            .client(namespace.as_str())
            .public_key()
            .await
            .unwrap();
        fixture.server.upload(namespace.as_str(), &archive).await;
        let accepted = native::import::Store::new();
        let result = accepted.copy(
            &format!("{}{namespace}", fixture.server.endpoint),
            fixture.credential.expose_bearer(),
            key.as_str(),
            &archive.path,
        );
        assert!(result.status.success(), "{:?}", result.stderr);
        assert_eq!(
            accepted.read(&archive.path).unwrap(),
            b"published after issuance"
        );
        let peer = native::import::Store::new();
        if namespace == fixture.credential.views().private() {
            let result = peer.copy(
                &format!("{}{namespace}", fixture.server.endpoint),
                fixture.peer.expose_bearer(),
                key.as_str(),
                &archive.path,
            );
            assert!(!result.status.success());
            assert!(peer.read(&archive.path).is_err());
        }
    }
}
