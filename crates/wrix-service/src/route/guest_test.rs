use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn apple_endpoint_routes_stay_stable_across_authorized_backend_changes() -> TestResult {
    tokio::time::timeout(Duration::from_secs(3), async {
        let public = Listeners::bind([Port::Available; 4]).await?;
        let host = public.address(Role::Sql)?;
        for role in Role::ALL {
            assert!(public.address(role)?.ip().is_loopback());
        }
        let guest = Listeners::bind([Port::Available; 4]).await?;
        let address = guest.address(Role::Sql)?;
        let ([blue, _, _, _, _], blue_route) = Listeners::bind([Port::Available; 4])
            .await?
            .into_generation()?;
        let ([green, _, _, _, _], green_route) = Listeners::bind([Port::Available; 4])
            .await?
            .into_generation()?;
        let front = FrontDoor::new(public, NonZeroUsize::MIN.saturating_add(3))?.with_guest(
            guest,
            crate::control::serving::apple::Address::loopback_for_test(),
        );
        let selected = front.selection();
        selected.select(blue_route.confirm_ready(Role::Sql))?;
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(front.serve(async {
            stopped.await.expect("shutdown sender");
        }));
        let mut existing = TcpStream::connect(address).await?;
        existing.write_all(b"a").await?;
        let (mut blue_stream, _) = blue.accept().await?;
        let mut byte = [0];
        blue_stream.read_exact(&mut byte).await?;
        assert_eq!(&byte, b"a");
        selected.select(green_route.confirm_ready(Role::Sql))?;
        let mut next = TcpStream::connect(address).await?;
        next.write_all(b"b").await?;
        let (mut green_stream, _) = green.accept().await?;
        green_stream.read_exact(&mut byte).await?;
        assert_eq!(&byte, b"b");
        blue_stream.write_all(b"c").await?;
        existing.read_exact(&mut byte).await?;
        assert_eq!(&byte, b"c");
        let mut host_client = TcpStream::connect(host).await?;
        host_client.write_all(b"d").await?;
        let (mut host_stream, _) = green.accept().await?;
        host_stream.read_exact(&mut byte).await?;
        assert_eq!(&byte, b"d");
        assert_eq!(blue_route.connections(Role::Sql), 1);
        assert_eq!(green_route.connections(Role::Sql), 2);
        drop((
            existing,
            next,
            host_client,
            blue_stream,
            green_stream,
            host_stream,
        ));
        shutdown.send(()).expect("live front door");
        serving.await??;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .await?
}

#[tokio::test]
async fn guest_routes_share_selection_and_retain_an_admitted_stream_while_admission_closes()
-> TestResult {
    let public = Listeners::bind([Port::Available; 4]).await?;
    let guest = Listeners::bind([Port::Available; 4]).await?;
    let address = guest.address(Role::Sql)?;
    let native = Listeners::bind([Port::Available; 4]).await?;
    let ([sql, _, _, _, _], generation) = native.into_generation()?;
    let front = FrontDoor::new(public, NonZeroUsize::MIN.saturating_add(3))?.with_guest(
        guest,
        crate::control::serving::apple::Address::loopback_for_test(),
    );
    let selected = front.selection();
    selected.select(generation.confirm_ready(Role::Sql))?;
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(front.serve(async {
        stopped.await.expect("shutdown sender");
    }));
    let mut client = TcpStream::connect(address).await?;
    client.write_all(b"first").await?;
    let (mut backend, _) = sql.accept().await?;
    let mut data = [0; 5];
    backend.read_exact(&mut data).await?;
    assert_eq!(&data, b"first");
    assert_eq!(generation.connections(Role::Sql), 1);
    selected.close(Role::Sql)?;
    let mut refused = TcpStream::connect(address).await?;
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), refused.read(&mut byte)).await??,
        0
    );
    backend.write_all(b"alive").await?;
    client.read_exact(&mut data).await?;
    assert_eq!(&data, b"alive");
    drop(client);
    drop(backend);
    shutdown.send(()).expect("live front door");
    serving.await??;
    assert_eq!(generation.connections(Role::Sql), 0);
    Ok(())
}

#[tokio::test]
async fn guest_listener_refuses_an_outside_subnet_peer_before_dispatch_to_a_live_backend()
-> TestResult {
    let public = Listeners::bind([Port::Available; 4]).await?;
    let guest = Listeners::bind([Port::Available; 4]).await?;
    let address = guest.address(Role::CacheRead)?;
    let host_address = public.address(Role::CacheRead)?;
    let native = Listeners::bind([Port::Available; 4]).await?;
    let ([_, cache, _, _, _], generation) = native.into_generation()?;
    let front = FrontDoor::new(public, NonZeroUsize::MIN.saturating_add(3))?.with_guest(
        guest,
        crate::control::serving::apple::Address::private_for_test(),
    );
    front
        .selection()
        .select(generation.confirm_ready(Role::CacheRead))?;
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(front.serve(async {
        stopped.await.expect("shutdown sender");
    }));
    let mut denied = TcpStream::connect(address).await?;
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), denied.read(&mut byte)).await??,
        0
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), cache.accept())
            .await
            .is_err()
    );
    assert_eq!(generation.connections(Role::CacheRead), 0);
    let mut live = TcpStream::connect(host_address).await?;
    live.write_all(b"x").await?;
    let (mut backend, _) = tokio::time::timeout(Duration::from_secs(1), cache.accept()).await??;
    backend.read_exact(&mut byte).await?;
    assert_eq!(&byte, b"x");
    drop(live);
    drop(backend);
    shutdown.send(()).expect("live front door");
    serving.await??;
    Ok(())
}
