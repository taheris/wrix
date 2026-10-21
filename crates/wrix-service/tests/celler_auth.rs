//! Native authorization after actual host-policy withdrawal, without a deployed worker.
#![cfg(test)]

use std::time::Duration;
use wrix_cache::backend::{issuer::Authority, pull::Audience};
use wrix_core::path::WorkspaceHash;
use wrix_service::control::{Access, Client, Controller, Namespace, Operation, Policy, Reply};

// Shared producer also supplies the backend suite's isolated-store and unsigned controls.
#[allow(dead_code)]
#[path = "../../wrix-cache/tests/common/celler/mod.rs"]
mod native;

async fn apply(controller: &Controller, client: &Client, operation: Operation) {
    let (served, reply) = tokio::join!(controller.serve_one(), client.request(operation));
    served.unwrap();
    assert_eq!(reply.unwrap(), Reply::Applied);
}

#[tokio::test]
async fn withdrawn_private_policy_denies_renewal_but_old_native_grants_read_new_publications() {
    let server = native::Server::start_with_issuer();
    let authority = Authority::open(&server.issuer_root()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let controller = Controller::bind(&root.path().join("control")).unwrap();
    let client = Client::open(&root.path().join("control")).unwrap();
    let workspace = WorkspaceHash::parse(&"a".repeat(64)).unwrap();
    let namespace = Namespace::from(&workspace);
    let mut policy = Policy {
        dolt: Access::Denied,
        private_cache_read: Access::Allowed,
        common_cache_read: Access::Allowed,
        egress: Access::Denied,
        compiler: Access::Denied,
    };
    apply(
        &controller,
        &client,
        Operation::Register {
            namespace: namespace.clone(),
            policy,
        },
    )
    .await;
    let original = controller.cache_credential(&authority, &workspace).unwrap();
    let views = original.views();
    let common = views.common().as_str();
    let private = views.private().as_str();
    let retention = Duration::from_hours(720);
    let common_key = server.client(common).register(retention).await.unwrap();
    let private_key = server.client(private).register(retention).await.unwrap();

    apply(
        &controller,
        &client,
        Operation::Retire {
            namespace: namespace.clone(),
        },
    )
    .await;
    policy.private_cache_read = Access::Denied;
    apply(
        &controller,
        &client,
        Operation::Register { namespace, policy },
    )
    .await;
    let renewed = controller.cache_credential(&authority, &workspace).unwrap();
    assert_eq!(renewed.views().audience(), Audience::Common);

    // Both payloads are created and published only after withdrawal and renewal.
    // A cached narinfo or pre-existing local path therefore cannot satisfy the assertions.
    for (view, key, bytes) in [
        (common, common_key, "new common after withdrawal"),
        (private, private_key, "new private after withdrawal"),
    ] {
        let archive = native::archive::Archive::file(bytes, "after-withdrawal");
        server.upload(view, &archive).await;
        let endpoint = format!("{}{view}", server.endpoint);
        let old_store = native::import::Store::new();
        let imported = old_store.copy(
            &endpoint,
            original.expose_bearer(),
            key.as_str(),
            &archive.path,
        );
        assert!(
            imported.status.success(),
            "{}",
            String::from_utf8_lossy(&imported.stderr)
        );
        assert_eq!(old_store.read(&archive.path).unwrap(), bytes.as_bytes());

        let renewed_store = native::import::Store::new();
        let imported = renewed_store.copy(
            &endpoint,
            renewed.expose_bearer(),
            key.as_str(),
            &archive.path,
        );
        if view == common {
            assert!(
                imported.status.success(),
                "{}",
                String::from_utf8_lossy(&imported.stderr)
            );
            assert_eq!(renewed_store.read(&archive.path).unwrap(), bytes.as_bytes());
        } else {
            assert!(!imported.status.success());
            assert_eq!(
                renewed_store.read(&archive.path).unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
            let denied = reqwest::Client::new()
                .get(format!("{endpoint}/nix-cache-info"))
                .bearer_auth(renewed.expose_bearer())
                .send()
                .await
                .unwrap();
            assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);
        }
    }
    controller.close().unwrap();
}
