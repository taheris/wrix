use super::*;
mod routing;
use crate::gateway::test::{Fixture, eof, get, request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn namespace() -> Namespace {
    Namespace::try_from("a".repeat(64)).unwrap()
}

#[tokio::test]
async fn live_handoff_preserves_blue_connect_and_green_policy_with_one_owner() {
    let blue = Fixture::start().await;
    let green = Fixture::start().await;
    let issuer = Issuer::new(blue.management.clone());
    let owner = issuer
        .issue_checked(namespace(), crate::gateway::test::policy())
        .await
        .unwrap();
    let secret = owner.credential().clone();
    let policy = owner.policy().clone();
    let mut tunnel = blue.connect().await;
    assert!(
        request(&mut tunnel, &secret, "allowed.example", "CONNECT", "")
            .await
            .starts_with("HTTP/1.1 200 ")
    );
    issuer.handoff(green.management.clone()).await.unwrap();
    assert_eq!(owner.credential(), &secret);
    assert_eq!(owner.policy(), &policy);
    let mut next = green.connect().await;
    get(&mut next, &secret).await;
    assert!(
        request(&mut next, &secret, "denied.example", "GET", "/")
            .await
            .starts_with("HTTP/1.1 403 ")
    );
    tunnel.write_all(b"still-blue\r\n\r\n").await.unwrap();
    let mut echoed = [0; 14];
    tunnel.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"still-blue\r\n\r\n");
    drop(owner);
    eof(&mut tunnel).await;
    eof(&mut next).await;
    blue.finish().await;
    green.finish().await;
}

#[tokio::test]
async fn confirmed_transfer_distinguishes_former_close_from_current_owner_loss() {
    let fixture = Fixture::start().await;
    let issuer = Issuer::new(fixture.management.clone());
    let owner = issuer
        .issue_checked(namespace(), crate::gateway::test::policy())
        .await
        .unwrap();
    let secret = owner.credential().clone();
    let candidate = owner.prepare().unwrap();
    let stale = owner.prepare().unwrap();
    let current = candidate.confirm(&owner).unwrap();
    assert!(stale.confirm(&owner).is_err());
    drop(owner);
    let mut socket = fixture.connect().await;
    get(&mut socket, &secret).await;
    drop(current);
    eof(&mut socket).await;
    fixture.finish().await;
}

#[tokio::test]
async fn control_loss_before_confirmation_invalidates_candidate() {
    let fixture = Fixture::start().await;
    let issuer = Issuer::new(fixture.management.clone());
    let owner = issuer
        .issue_checked(namespace(), crate::gateway::test::policy())
        .await
        .unwrap();
    let candidate = owner.prepare().unwrap();
    let secret = owner.credential().clone();
    owner.live.revoke();
    assert!(candidate.confirm(&owner).is_err());
    assert!(owner.prepare().is_err());
    let mut socket = fixture.connect().await;
    for _ in 0..100 {
        if fixture.management.status().await.unwrap().grants == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        request(&mut socket, &secret, "allowed.example", "GET", "/")
            .await
            .starts_with("HTTP/1.1 407 ")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn failed_green_preparation_does_not_authorize_or_revoke_blue() {
    let blue = Fixture::start().await;
    let green = Fixture::start().await;
    let issuer = Issuer::new(blue.management.clone());
    let owner = issuer
        .issue_checked(namespace(), crate::gateway::test::policy())
        .await
        .unwrap();
    let secret = owner.credential().clone();
    green.management.drain(true).await.unwrap();
    assert!(issuer.handoff(green.management.clone()).await.is_err());
    get(&mut blue.connect().await, &secret).await;
    green.management.drain(false).await.unwrap();
    let mut unknown = green.connect().await;
    assert!(
        request(&mut unknown, &secret, "allowed.example", "GET", "/")
            .await
            .starts_with("HTTP/1.1 407 ")
    );
    blue.finish().await;
    green.finish().await;
}

#[tokio::test]
async fn transferred_owner_loss_revokes_both_generations_without_revoking_peer() {
    let blue = Fixture::start().await;
    let green = Fixture::start().await;
    let issuer = Issuer::new(blue.management.clone());
    let owner = issuer
        .issue_checked(namespace(), crate::gateway::test::policy())
        .await
        .unwrap();
    let secret = owner.credential().clone();
    let peer = issuer
        .issue_checked(namespace(), crate::gateway::test::policy())
        .await
        .unwrap();
    let peer_secret = peer.credential().clone();
    let mut old = blue.connect().await;
    get(&mut old, &secret).await;
    issuer.handoff(green.management.clone()).await.unwrap();
    issuer.handoff(green.management.clone()).await.unwrap();
    let mut new = green.connect().await;
    get(&mut new, &secret).await;
    drop(owner);
    eof(&mut old).await;
    eof(&mut new).await;
    get(&mut green.connect().await, &peer_secret).await;
    blue.finish().await;
    green.finish().await;
}

#[tokio::test]
async fn partially_failed_preparation_never_authorizes_either_green_grant() {
    let blue = Fixture::start().await;
    let green = Fixture::start().await;
    let issuer = Issuer::new(blue.management.clone());
    let first = issuer
        .issue_checked(namespace(), crate::gateway::test::policy())
        .await
        .unwrap();
    let second = issuer
        .issue_checked(namespace(), crate::gateway::test::policy())
        .await
        .unwrap();
    let _conflict = green
        .management
        .prepare(second.credential().clone(), second.policy().clone())
        .await
        .unwrap();
    assert!(issuer.handoff(green.management.clone()).await.is_err());
    for secret in [first.credential(), second.credential()] {
        assert!(
            request(
                &mut green.connect().await,
                secret,
                "allowed.example",
                "GET",
                "/"
            )
            .await
            .starts_with("HTTP/1.1 407 ")
        );
        get(&mut blue.connect().await, secret).await;
    }
    blue.finish().await;
    green.finish().await;
}

#[tokio::test]
async fn launcher_control_eof_revokes_without_application_heartbeat() {
    let fixture = Fixture::start().await;
    let issuer = Issuer::new(fixture.management.clone());
    let owner = issuer
        .issue_checked(namespace(), crate::gateway::test::policy())
        .await
        .unwrap();
    let secret = owner.credential().clone();
    let (controller, launcher) = tokio::net::UnixStream::pair().unwrap();
    let observing = tokio::spawn(owner.observe(controller));
    let mut socket = fixture.connect().await;
    get(&mut socket, &secret).await;
    drop(launcher);
    observing.await.unwrap().unwrap();
    eof(&mut socket).await;
    fixture.finish().await;
}

#[tokio::test]
async fn drained_blue_retirement_detaches_old_coordination_not_transferred_authority() {
    let blue = Fixture::start().await;
    let green = Fixture::start().await;
    let issuer = Issuer::new(blue.management.clone());
    let owner = issuer
        .issue_checked(namespace(), crate::gateway::test::policy())
        .await
        .unwrap();
    let secret = owner.credential().clone();
    let mut old = blue.connect().await;
    get(&mut old, &secret).await;
    issuer.handoff(green.management.clone()).await.unwrap();
    assert!(issuer.release_drained(&blue.management).await.is_err());
    blue.management.drain(true).await.unwrap();
    assert!(issuer.release_drained(&blue.management).await.is_err());
    drop(old);
    for _ in 0..100 {
        if blue.management.status().await.unwrap().connections == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    issuer.release_drained(&blue.management).await.unwrap();
    blue.finish().await;
    get(&mut green.connect().await, &secret).await;
    assert!(owner.prepare().is_ok());
    green.finish().await;
}
