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
        assert_eq!(original.expose_bearer(), original_bytes);
    }
    let peer = WorkspaceHash::parse(&"b".repeat(64)).unwrap();
    assert!(matches!(
        controller.cache_credential(&issuer, &peer),
        Err(Error::Control(crate::control::Error::UnknownNamespace))
    ));
    controller.close().unwrap();
}
