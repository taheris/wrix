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

#[tokio::test]
async fn native_nix_reloads_atomic_session_netrc_using_current_controller_policy() {
    use std::{
        fs,
        net::{IpAddr, Ipv4Addr},
        sync::{Arc, Mutex},
        time::SystemTime,
    };
    use wrix_sandbox::cache_credentials::{Clock, Error, Grant, Issuer, Renewal, Session, Time};

    struct LiveIssuer<'a> {
        controller: &'a Controller,
        authority: &'a Authority,
        workspace: &'a WorkspaceHash,
    }
    impl Issuer for LiveIssuer<'_> {
        fn issue(&mut self) -> Result<Grant, Error> {
            let credential = self
                .controller
                .cache_credential(self.authority, self.workspace)
                .map_err(|_| Error::Unavailable)?;
            Grant::new(
                credential.expose_bearer().to_owned(),
                credential.expires_at(),
            )
        }
    }
    #[derive(Clone)]
    struct HostClock(Arc<Mutex<SystemTime>>);
    impl Clock for HostClock {
        fn now(&self) -> Time {
            Time {
                wall: *self.0.lock().unwrap(),
                monotonic: Duration::ZERO,
            }
        }
    }

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
    let views = wrix_cache::backend::pull::ReadSet::for_workspace(&workspace).unwrap();
    let private = views.private().as_str();
    let common = views.common().as_str();
    let private_key = server
        .client(private)
        .register(Duration::from_hours(720))
        .await
        .unwrap();
    let common_key = server
        .client(common)
        .register(Duration::from_hours(720))
        .await
        .unwrap();
    let clock = HostClock(Arc::new(Mutex::new(SystemTime::now())));
    let mut session = Session::create(
        LiveIssuer {
            controller: &controller,
            authority: &authority,
            workspace: &workspace,
        },
        clock.clone(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
    )
    .unwrap();
    let netrc = session.netrc();
    let initial_credential = fs::read(&netrc).unwrap();
    let archive = native::archive::Archive::file("before renewal", "before-netrc-renewal");
    server.upload(private, &archive).await;
    let before = native::import::Store::new();
    let imported = before.copy_with_netrc(
        &format!("{}{private}", server.endpoint),
        &netrc,
        private_key.as_str(),
        &archive.path,
    );
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    assert_eq!(before.read(&archive.path).unwrap(), b"before renewal");

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
    *clock.0.lock().unwrap() = session.expires_at() - Duration::from_mins(1);
    assert_eq!(session.tick(), Renewal::Replaced);
    assert_eq!(session.netrc(), netrc);
    let credential_changed = fs::read(&netrc).unwrap() != initial_credential;
    assert!(credential_changed);
    for (view, key, permitted) in [(common, common_key, true), (private, private_key, false)] {
        let archive = native::archive::Archive::file(view, "after-netrc-renewal");
        server.upload(view, &archive).await;
        let store = native::import::Store::new();
        let imported = store.copy_with_netrc(
            &format!("{}{view}", server.endpoint),
            &netrc,
            key.as_str(),
            &archive.path,
        );
        assert_eq!(
            imported.status.success(),
            permitted,
            "{}",
            String::from_utf8_lossy(&imported.stderr)
        );
        if permitted {
            assert_eq!(store.read(&archive.path).unwrap(), view.as_bytes());
        } else {
            assert_eq!(
                store.read(&archive.path).unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
        }
    }
    let directory = session.directory();
    drop(session);
    assert!(!directory.exists());
    controller.close().unwrap();
}
