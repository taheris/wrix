use super::*;
use crate::route::{FrontDoor, Listeners, Port, Role};
use std::{num::NonZeroUsize, sync::atomic::Ordering};
use tokio::net::TcpStream;

#[tokio::test]
async fn unchanged_host_endpoint_pins_blue_stream_and_routes_new_requests_to_green() {
    let (blue_listeners, blue_route) = Listeners::bind([Port::Available; 4])
        .await
        .unwrap()
        .into_generation()
        .unwrap();
    let [_, _, egress, _] = blue_listeners;
    let blue = Fixture::on_listener(egress.into_inner(), 32).await;
    let (green_listeners, green_route) = Listeners::bind([Port::Available; 4])
        .await
        .unwrap()
        .into_generation()
        .unwrap();
    let [_, _, egress, _] = green_listeners;
    let green = Fixture::on_listener(egress.into_inner(), 32).await;
    let front = FrontDoor::new(
        Listeners::bind([Port::Available; 4]).await.unwrap(),
        NonZeroUsize::new(32).unwrap(),
    )
    .unwrap();
    let stable = front.address(Role::Egress).unwrap();
    let selection = front.selection();
    blue.management.verify(blue.data).await.unwrap();
    selection
        .select(blue_route.confirm_ready(Role::Egress))
        .unwrap();
    let routing = tokio::spawn(front.serve(std::future::pending()));
    let issuer = Issuer::new(blue.management.clone());
    let owner = issuer
        .issue_checked(namespace(), crate::gateway::test::policy())
        .await
        .unwrap();
    let secret = owner.credential().clone();
    let mut old = TcpStream::connect(stable).await.unwrap();
    assert!(
        request(&mut old, &secret, "allowed.example", "CONNECT", "")
            .await
            .starts_with("HTTP/1.1 200 ")
    );
    green.management.verify(green.data).await.unwrap();
    issuer.handoff(green.management.clone()).await.unwrap();
    selection
        .select(green_route.confirm_ready(Role::Egress))
        .unwrap();
    let mut new = TcpStream::connect(stable).await.unwrap();
    get(&mut new, &secret).await;
    assert_eq!(blue.network.connections.load(Ordering::SeqCst), 1);
    assert_eq!(green.network.connections.load(Ordering::SeqCst), 1);
    old.write_all(b"old-route\r\n\r\n").await.unwrap();
    let mut body = [0; 13];
    old.read_exact(&mut body).await.unwrap();
    assert_eq!(&body, b"old-route\r\n\r\n");
    drop(owner);
    eof(&mut old).await;
    eof(&mut new).await;
    routing.abort();
    assert!(routing.await.unwrap_err().is_cancelled());
    blue.finish().await;
    green.finish().await;
}
