use std::{net::SocketAddr, num::NonZeroUsize, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use wrix_service::route::{FrontDoor, Generation, Listeners, Port, Role};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult<T = ()> = Result<T, TestError>;

const BUDGET: Duration = Duration::from_secs(10);

struct Peers {
    generation: Generation,
    addresses: [SocketAddr; 4],
    task: JoinHandle<TestResult>,
}

impl Peers {
    async fn start(color: u8) -> TestResult<Self> {
        let listeners = Listeners::bind([Port::Available; 4]).await?;
        let [a, b, c, d] = Role::ALL.map(|role| listeners.address(role));
        let addresses = [a?, b?, c?, d?];
        let (listeners, generation) = listeners.into_generation()?;
        let task = tokio::spawn(async move {
            let mut servers = JoinSet::new();
            for (index, listener) in listeners.into_iter().enumerate() {
                servers.spawn(serve_peer(listener, [color, u8::try_from(index)?]));
            }
            while let Some(result) = servers.join_next().await {
                result??;
            }
            Ok(())
        });
        Ok(Self {
            generation,
            addresses,
            task,
        })
    }
}

async fn serve_peer(listener: rama_tcp::server::TcpListener, greeting: [u8; 2]) -> TestResult {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (mut stream, _) = accepted?;
                connections.spawn(async move {
                    stream.write_all(&greeting).await?;
                    let (mut reader, mut writer) = tokio::io::split(stream);
                    tokio::io::copy(&mut reader, &mut writer).await?;
                    writer.shutdown().await?;
                    Ok::<(), TestError>(())
                });
            }
            result = connections.join_next(), if !connections.is_empty() => {
                if let Some(result) = result { result??; }
            }
        }
    }
}

impl Drop for Peers {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn connect(address: SocketAddr, expected: [u8; 2]) -> TestResult<TcpStream> {
    let mut stream = TcpStream::connect(address).await?;
    let mut greeting = [0; 2];
    stream.read_exact(&mut greeting).await?;
    assert_eq!(greeting, expected);
    Ok(stream)
}

async fn echo(stream: &mut TcpStream, bytes: &[u8]) -> TestResult {
    stream.write_all(bytes).await?;
    let mut received = vec![0; bytes.len()];
    stream.read_exact(&mut received).await?;
    assert_eq!(received, bytes);
    Ok(())
}

async fn rejected(address: SocketAddr) -> TestResult {
    let mut stream = TcpStream::connect(address).await?;
    let mut byte = [0];
    match stream.read(&mut byte).await {
        Ok(count) => assert_eq!(count, 0, "closed route returned bytes"),
        Err(error) => assert!(matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
        )),
    }
    Ok(())
}

#[tokio::test]
async fn creation_rejects_occupied_fixed_ports_and_excessive_capacity() {
    let occupied = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = std::num::NonZeroU16::new(occupied.local_addr().unwrap().port()).unwrap();
    assert!(matches!(
        Listeners::bind([
            Port::Available,
            Port::Fixed(port),
            Port::Available,
            Port::Available
        ])
        .await,
        Err(wrix_service::route::Error::Io(_))
    ));
    let listeners = Listeners::bind([Port::Available; 4]).await.unwrap();
    assert!(matches!(
        FrontDoor::new(listeners, NonZeroUsize::new(1025).unwrap()),
        Err(wrix_service::route::Error::CapacityExceeded)
    ));
}

#[tokio::test]
async fn local_peers_greet_echo_and_preserve_half_close() {
    timeout(BUDGET, async {
        let peers = Peers::start(b'P').await.unwrap();
        for (index, address) in peers.addresses.iter().enumerate() {
            let mut stream = connect(*address, [b'P', u8::try_from(index).unwrap()])
                .await
                .unwrap();
            echo(&mut stream, b"native peer positive control")
                .await
                .unwrap();
            stream.shutdown().await.unwrap();
            assert_eq!(stream.read(&mut [0]).await.unwrap(), 0);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn fixed_role_routes_preserve_connections_without_general_proxying() {
    timeout(BUDGET, async {
        let blue = Peers::start(b'B').await.unwrap();
        let green = Peers::start(b'G').await.unwrap();
        let listeners = Listeners::bind([Port::Available; 4]).await.unwrap();
        let addresses = Role::ALL.map(|role| listeners.address(role).unwrap());
        assert!(
            addresses
                .iter()
                .all(|address| address.ip().is_loopback() && address.port() != 0)
        );
        let front = FrontDoor::new(listeners, NonZeroUsize::new(16).unwrap()).unwrap();
        let selection = front.selection();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(front.serve(async {
            stopped.await.unwrap();
        }));
        for address in addresses {
            rejected(address).await.unwrap();
        }
        for role in Role::ALL {
            selection
                .select(blue.generation.confirm_ready(role))
                .unwrap();
        }
        let mut old = Vec::new();
        for (index, address) in addresses.iter().enumerate() {
            old.push(
                connect(*address, [b'B', u8::try_from(index).unwrap()])
                    .await
                    .unwrap(),
            );
        }
        for role in Role::ALL {
            assert_eq!(blue.generation.connections(role), 1);
        }
        for role in Role::ALL {
            selection
                .select(green.generation.confirm_ready(role))
                .unwrap();
        }
        for (index, address) in addresses.iter().enumerate() {
            let mut new = connect(*address, [b'G', u8::try_from(index).unwrap()])
                .await
                .unwrap();
            echo(&mut new, b"new connection uses selected generation")
                .await
                .unwrap();
            echo(
                &mut old[index],
                b"old native session still belongs to Blue\0\xff",
            )
            .await
            .unwrap();
        }
        for role in Role::ALL {
            selection.close(role).unwrap();
        }
        for address in addresses {
            rejected(address).await.unwrap();
        }
        for stream in &mut old {
            echo(stream, b"closing new admission does not close old work")
                .await
                .unwrap();
        }
        stop.send(()).unwrap();
        for stream in &mut old {
            echo(stream, b"listener shutdown still drains existing streams")
                .await
                .unwrap();
        }
        drop(old);
        task.await.unwrap().unwrap();
        for role in Role::ALL {
            assert_eq!(blue.generation.connections(role), 0);
            assert_eq!(green.generation.connections(role), 0);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn client_bytes_cannot_choose_destinations_or_reach_control() {
    timeout(BUDGET, async {
        let peers = Peers::start(b'D').await.unwrap();
        let forbidden = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let forbidden_address = forbidden.local_addr().unwrap();
        let mut probe = TcpStream::connect(forbidden_address).await.unwrap();
        let (mut control, _) = forbidden.accept().await.unwrap();
        probe.write_all(b"live control peer").await.unwrap();
        let mut observed = [0; 17];
        control.read_exact(&mut observed).await.unwrap();
        assert_eq!(&observed, b"live control peer");
        drop((probe, control));
        let listeners = Listeners::bind([Port::Available; 4]).await.unwrap();
        let addresses = Role::ALL.map(|role| listeners.address(role).unwrap());
        let front = FrontDoor::new(listeners, NonZeroUsize::new(4).unwrap()).unwrap();
        let selection = front.selection();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(front.serve(async { stopped.await.unwrap(); }));
        for (index, role) in Role::ALL.into_iter().enumerate() {
            selection.select(peers.generation.confirm_ready(role)).unwrap();
            let mut stream = connect(addresses[index], [b'D', u8::try_from(index).unwrap()]).await.unwrap();
            let payload = format!("CONNECT {forbidden_address} HTTP/1.1\r\nHost: {forbidden_address}\r\n\r\nPOST /control/select\nSELECT * FROM mysql.user;\0");
            echo(&mut stream, payload.as_bytes()).await.unwrap();
        }
        assert!(timeout(Duration::from_millis(50), forbidden.accept()).await.is_err());
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    }).await.unwrap();
}

#[tokio::test]
async fn failed_connect_does_not_fallback_or_break_existing_stream() {
    timeout(BUDGET, async {
        let peers = Peers::start(b'B').await.unwrap();
        let listeners = Listeners::bind([Port::Available; 4]).await.unwrap();
        let address = listeners.address(Role::Sql).unwrap();
        let front = FrontDoor::new(listeners, NonZeroUsize::new(2).unwrap()).unwrap();
        let selection = front.selection();
        selection
            .select(peers.generation.confirm_ready(Role::Sql))
            .unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(front.serve(async {
            stopped.await.unwrap();
        }));
        let mut old = connect(address, [b'B', 0]).await.unwrap();
        let (unserved, unavailable) = Listeners::bind([Port::Available; 4])
            .await
            .unwrap()
            .into_generation()
            .unwrap();
        drop(unserved);
        selection
            .select(unavailable.confirm_ready(Role::Sql))
            .unwrap();
        rejected(address).await.unwrap();
        assert_eq!(unavailable.connections(Role::Sql), 0);
        echo(&mut old, b"failed Green connect cannot affect Blue")
            .await
            .unwrap();
        selection
            .select(peers.generation.confirm_ready(Role::Sql))
            .unwrap();
        drop(connect(address, [b'B', 0]).await.unwrap());
        drop(old);
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn capacity_rejects_excess_streams_without_interrupting_admitted_work() {
    timeout(BUDGET, async {
        let peers = Peers::start(b'B').await.unwrap();
        let listeners = Listeners::bind([Port::Available; 4]).await.unwrap();
        let address = listeners.address(Role::Compiler).unwrap();
        let front = FrontDoor::new(listeners, NonZeroUsize::new(1).unwrap()).unwrap();
        front
            .selection()
            .select(peers.generation.confirm_ready(Role::Compiler))
            .unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(front.serve(async {
            stopped.await.unwrap();
        }));
        let mut first = connect(address, [b'B', 3]).await.unwrap();
        rejected(address).await.unwrap();
        assert_eq!(peers.generation.connections(Role::Compiler), 1);
        let bytes = vec![0x97; 2 * 1024 * 1024 + 7];
        let (mut reader, mut writer) = first.split();
        let ((), received) = tokio::join!(
            async {
                writer.write_all(&bytes).await.unwrap();
                writer.shutdown().await.unwrap();
            },
            async {
                let mut received = Vec::new();
                reader.read_to_end(&mut received).await.unwrap();
                received
            }
        );
        assert_eq!(received, bytes);
        drop(first);
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(peers.generation.connections(Role::Compiler), 0);
    })
    .await
    .unwrap();
}
