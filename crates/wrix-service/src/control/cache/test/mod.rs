use super::*;
use crate::control::{Client, Operation, Policy, Reply};

async fn apply(controller: &Controller, client: &Client, operation: Operation) {
    let (served, reply) = tokio::join!(controller.serve_one(), client.request(operation));
    served.unwrap();
    assert_eq!(reply.unwrap(), Reply::Applied);
}

#[tokio::test]
async fn renewal_uses_current_host_policy_and_never_replays_withdrawn_audiences() {
    let root = tempfile::tempdir().unwrap();
    let metadata = root.path().join("verification");
    std::fs::create_dir(&metadata).unwrap();
    let issuer = Authority::provision(&root.path().join("issuer"), &metadata).unwrap();
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
    assert!(matches!(
        controller.cache_credential(&issuer, &workspace),
        Err(Error::Control(crate::control::Error::UnknownNamespace))
    ));
    apply(
        &controller,
        &client,
        Operation::Register {
            namespace: namespace.clone(),
            policy,
        },
    )
    .await;
    let original = controller.cache_credential(&issuer, &workspace).unwrap();
    assert_eq!(original.views().audience(), Audience::CommonAndPrivate);
    let original_bytes = original.expose_bearer().to_owned();

    for (common, private, expected) in [
        (Access::Allowed, Access::Denied, Some(Audience::Common)),
        (Access::Denied, Access::Allowed, Some(Audience::Private)),
        (Access::Denied, Access::Denied, None),
    ] {
        apply(
            &controller,
            &client,
            Operation::Retire {
                namespace: namespace.clone(),
            },
        )
        .await;
        assert!(matches!(
            controller.cache_credential(&issuer, &workspace),
            Err(Error::Control(crate::control::Error::UnknownNamespace))
        ));
        policy.common_cache_read = common;
        policy.private_cache_read = private;
        apply(
            &controller,
            &client,
            Operation::Register {
                namespace: namespace.clone(),
                policy,
            },
        )
        .await;
        match expected {
            Some(audience) => assert_eq!(
                controller
                    .cache_credential(&issuer, &workspace)
                    .unwrap()
                    .views()
                    .audience(),
                audience
            ),
            None => assert!(matches!(
                controller.cache_credential(&issuer, &workspace),
                Err(Error::Control(crate::control::Error::PermissionDenied))
            )),
        }
        let original_unchanged = original.expose_bearer() == original_bytes;
        assert!(original_unchanged);
    }
    let peer = WorkspaceHash::parse(&"b".repeat(64)).unwrap();
    assert!(matches!(
        controller.cache_credential(&issuer, &peer),
        Err(Error::Control(crate::control::Error::UnknownNamespace))
    ));
    controller.close().unwrap();
}

#[tokio::test]
async fn private_issuance_never_provisions_missing_issuer_authority() {
    let root = tempfile::tempdir().unwrap();
    let issuer = root.path().join("missing-issuer");
    let controller = Controller::bind(&root.path().join("control"))
        .unwrap()
        .with_cache_issuer(&issuer)
        .unwrap();
    let client = Client::open(&root.path().join("control")).unwrap();
    let namespace = Namespace::from(&WorkspaceHash::parse(&"a".repeat(64)).unwrap());
    let (served, grant) = tokio::join!(controller.serve_one(), client.cache_credential(namespace));
    assert!(matches!(
        served,
        Err(crate::control::Error::CacheUnavailable)
    ));
    assert!(grant.is_err());
    assert!(!issuer.exists());
    controller.close().unwrap();
}

#[tokio::test]
async fn private_credential_frames_are_bounded_before_body_allocation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("control");
    Controller::bind(&root).unwrap().close().unwrap();
    let client = Client::open(&root).unwrap();
    let listener = tokio::net::UnixListener::bind(root.join("control.sock")).unwrap();
    let namespace = Namespace::from(&WorkspaceHash::parse(&"a".repeat(64)).unwrap());
    let server = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: crate::control::Request = crate::control::receive(&mut stream).await.unwrap();
        assert_eq!(request.version, crate::control::VERSION);
        assert!(matches!(request.operation, Operation::IssueCache { .. }));
        let authorized =
            request.management.as_slice() == std::fs::read(root.join("management.key")).unwrap();
        assert!(authorized);
        crate::control::send(
            &mut stream,
            &Reply::CacheIssued {
                expires_at: 1_000_000,
            },
        )
        .await
        .unwrap();
        stream
            .write_u32(u32::try_from(MAX_BEARER + 1).unwrap())
            .await
            .unwrap();
    };
    let ((), grant) = tokio::join!(server, client.cache_credential(namespace));
    assert!(matches!(grant, Err(crate::control::Error::FrameLimit)));
}
