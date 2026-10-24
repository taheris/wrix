#[cfg(test)]
#[path = "common/supervision/mod.rs"]
mod supervision;

use std::{net::SocketAddr, num::NonZeroUsize, time::Duration};

#[test]
fn supervision_preserves_stop_intent_and_active_closures() {
    supervision::exercise();
}

#[tokio::test]
async fn clean_controller_stop_releases_endpoint_without_rotating_private_authority() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap().join("authority");
    let controller = Controller::bind(&root).unwrap();
    let credential = std::fs::read(root.join("management.key")).unwrap();
    assert!(Controller::bind(&root).is_err());
    controller.close().unwrap();
    assert!(!root.join("control.sock").exists());
    let resumed = Controller::bind(&root).unwrap();
    assert_eq!(
        std::fs::read(root.join("management.key")).unwrap(),
        credential
    );
    resumed.close().unwrap();
}

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use wrix_service::route::{FrontDoor, Generation, Listeners, Port, Role};

use wrix_service::control::{
    Access, Client, Controller, Namespace, Operation, Permission, Policy, Reply,
};

#[tokio::test]
async fn private_controller_process_is_singleton_and_resumes_after_clean_stop() {
    struct Process(std::process::Child);
    impl Drop for Process {
        fn drop(&mut self) {
            if self.0.try_wait().unwrap().is_none() {
                self.0.kill().unwrap();
                self.0.wait().unwrap();
            }
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let start = || {
        Process(
            std::process::Command::new(env!("CARGO_BIN_EXE_wrix-host-controller"))
                .arg(&root)
                .spawn()
                .unwrap(),
        )
    };
    for _ in 0..2 {
        let mut owner = start();
        timeout(BUDGET, async {
            while !root.join("authority/control.sock").exists() {
                assert!(owner.0.try_wait().unwrap().is_none());
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let reply = Client::open(&root.join("authority"))
            .unwrap()
            .request(Operation::Inspect {})
            .await
            .unwrap();
        assert!(matches!(reply, Reply::Snapshot { version: 2, .. }));
        let endpoints: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("authority/role-endpoints.json")).unwrap(),
        )
        .unwrap();
        for port in endpoints["ports"].as_array().unwrap() {
            let port = u16::try_from(port.as_u64().unwrap()).unwrap();
            timeout(BUDGET, rejected(([127, 0, 0, 1], port).into()))
                .await
                .unwrap()
                .unwrap();
        }
        let mut competitor = start();
        assert!(!competitor.0.wait().unwrap().success());
        assert!(
            std::process::Command::new("python3")
                .args([
                    "-c",
                    "import os,signal,sys; os.kill(int(sys.argv[1]), signal.SIGTERM)",
                    &owner.0.id().to_string()
                ])
                .status()
                .unwrap()
                .success()
        );
        assert!(owner.0.wait().unwrap().success());
        assert!(!root.join("authority/control.sock").exists());
    }
}

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

fn namespace(digit: char) -> Result<Namespace, wrix_core::path::WorkspaceHashParseError> {
    Namespace::try_from(
        digit
            .to_string()
            .repeat(wrix_core::path::WorkspaceHash::HEX_LEN),
    )
}

const POLICY: Policy = Policy {
    dolt: Access::Allowed,
    private_cache_read: Access::Allowed,
    common_cache_read: Access::Denied,
    egress: Access::Allowed,
    compiler: Access::Denied,
};

async fn control_request(
    controller: &Controller,
    client: &Client,
    operation: Operation,
) -> TestResult<Reply> {
    let (served, reply) = tokio::join!(controller.serve_one(), client.request(operation));
    served?;
    Ok(reply?)
}

async fn raw_control(
    controller: &Controller,
    root: &std::path::Path,
    request: serde_json::Value,
) -> Result<serde_json::Value, wrix_service::control::Error> {
    let exchange = async {
        let mut stream = tokio::net::UnixStream::connect(root.join("control.sock")).await?;
        let bytes = serde_json::to_vec(&request)?;
        stream
            .write_u32(
                u32::try_from(bytes.len()).map_err(|_| wrix_service::control::Error::FrameLimit)?,
            )
            .await?;
        stream.write_all(&bytes).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        Ok::<_, wrix_service::control::Error>(response)
    };
    let (served, response) = tokio::join!(controller.serve_one(), exchange);
    served?;
    let response = response?;
    Ok(serde_json::from_slice(&response[4..])?)
}

fn envelope(
    root: &std::path::Path,
    operation: &serde_json::Value,
) -> TestResult<serde_json::Value> {
    Ok(
        serde_json::json!({"version":2, "management":std::fs::read(root.join("management.key"))?, "operation":operation}),
    )
}

#[tokio::test]
async fn control_operations_are_typed_authenticated_and_host_only() {
    timeout(BUDGET, async {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap().join("controller");
        let controller = Controller::bind(&root).unwrap();
        assert!(
            Controller::bind(&root).is_err(),
            "competing owner acquired control"
        );
        let client = Client::open(&root).unwrap();
        let operation = Operation::Register {
            namespace: namespace('a').unwrap(),
            policy: Policy {
                dolt: Access::Denied,
                ..POLICY
            },
        };
        assert_eq!(
            control_request(&controller, &client, operation.clone())
                .await
                .unwrap(),
            Reply::Applied
        );
        assert_eq!(
            control_request(&controller, &client, operation)
                .await
                .unwrap(),
            Reply::Applied
        );
        let conflicting = Operation::Register {
            namespace: namespace('a').unwrap(),
            policy: Policy {
                compiler: Access::Allowed,
                ..POLICY
            },
        };
        assert_eq!(
            control_request(&controller, &client, conflicting)
                .await
                .unwrap(),
            Reply::Conflict
        );
        let peer = Operation::Register {
            namespace: namespace('b').unwrap(),
            policy: POLICY,
        };
        assert_eq!(
            control_request(&controller, &client, peer).await.unwrap(),
            Reply::Applied
        );
        let mut bad_auth = envelope(
            &root,
            &serde_json::json!({"operation":"retire", "namespace":namespace('a').unwrap()}),
        )
        .unwrap();
        bad_auth["management"] = serde_json::json!([0; 32].to_vec());
        assert!(matches!(
            raw_control(&controller, &root, bad_auth).await,
            Err(wrix_service::control::Error::Unauthorized)
        ));
        let mut incompatible = envelope(
            &root,
            &serde_json::json!({"operation":"retire", "namespace":namespace('a').unwrap()}),
        )
        .unwrap();
        incompatible["version"] = serde_json::json!(99);
        assert_eq!(
            raw_control(&controller, &root, incompatible).await.unwrap(),
            serde_json::json!({"result":"incompatible"})
        );
        assert_eq!(
            control_request(
                &controller,
                &client,
                Operation::Retire {
                    namespace: namespace('a').unwrap()
                }
            )
            .await
            .unwrap(),
            Reply::Applied
        );
        let Reply::Snapshot {
            namespaces,
            version,
        } = control_request(&controller, &client, Operation::Inspect {})
            .await
            .unwrap()
        else {
            panic!("missing snapshot")
        };
        assert_eq!(version, 2);
        assert_eq!(namespaces.len(), 1);
        assert_eq!(namespaces.get(&namespace('b').unwrap()), Some(&POLICY));
        assert!(matches!(
            controller.authorize(&namespace('a').unwrap(), Permission::Sql),
            Err(wrix_service::control::Error::UnknownNamespace)
        ));
        assert!(matches!(
            controller.authorize(&namespace('b').unwrap(), Permission::Compiler),
            Err(wrix_service::control::Error::PermissionDenied)
        ));
        assert!(matches!(
            controller.authorize(&namespace('b').unwrap(), Permission::CommonCacheRead),
            Err(wrix_service::control::Error::PermissionDenied)
        ));
        let authorization = controller
            .authorize(&namespace('b').unwrap(), Permission::PrivateCacheRead)
            .unwrap();
        assert_eq!(authorization.namespace(), &namespace('b').unwrap());
        assert_eq!(authorization.permission(), Permission::PrivateCacheRead);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn private_authority_is_not_delegated_to_service_observations() {
    use std::os::unix::fs::PermissionsExt;
    timeout(BUDGET, async {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap().join("controller");
        let controller = Controller::bind(&root).unwrap();
        let client = Client::open(&root).unwrap();
        for operation in [
            serde_json::json!({"operation":"execute", "command":"true"}),
            serde_json::json!({"operation":"inspect", "path":"/nix/store"}),
            serde_json::json!({"operation":"inspect", "mounts":["/home"]}),
            serde_json::json!({"operation":"inspect", "destination":"127.0.0.1:22"}),
            serde_json::json!({"operation":"release_owner", "role":"sql", "container_report":"stopped"}),
            serde_json::json!({"operation":"register", "namespace":"../peer", "policy": POLICY}),
            serde_json::json!({"operation":"register", "namespace":namespace('a').unwrap(), "policy": {"dolt":"allowed", "private_cache_read":"allowed", "common_cache_read":"denied", "egress":"allowed", "compiler":"denied", "management":true}}),
            serde_json::json!({"operation":"inspect", "workspace_pointer":{"host_authority":true}}),
        ] {
            assert!(matches!(raw_control(&controller, &root, envelope(&root, &operation).unwrap()).await, Err(wrix_service::control::Error::Encoding(_))));
            assert!(Controller::bind(&root).is_err(), "untrusted report released ownership");
        }
        let snapshot = control_request(&controller, &client, Operation::Inspect {}).await.unwrap();
        assert_eq!(serde_json::to_value(snapshot).unwrap(), serde_json::json!({"result":"snapshot", "version":2, "namespaces":{}}));
        assert_eq!(std::fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o700);
        for name in ["management.key", "ownership", "control.sock"] {
            assert_eq!(std::fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }).await.unwrap();
}

#[tokio::test]
async fn private_control_rejects_oversized_frames_before_allocation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap().join("controller");
    let controller = Controller::bind(&root).unwrap();
    let exchange = async {
        let mut stream = tokio::net::UnixStream::connect(root.join("control.sock"))
            .await
            .unwrap();
        stream.write_u32(u32::MAX).await.unwrap();
        // Darwin peer credentials require the connecting process to retain its endpoint.
        std::future::pending::<()>().await;
    };
    tokio::select! {
        served = controller.serve_one() => {
            assert!(
                matches!(served, Err(wrix_service::control::Error::FrameLimit)),
                "{served:?}"
            );
        }
        () = exchange => panic!("oversized-frame peer must remain connected"),
        () = tokio::time::sleep(BUDGET) => panic!("oversized frame was not rejected"),
    }
}

#[tokio::test]
async fn concurrent_registration_cannot_overwrite_an_admitted_policy() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap().join("controller");
    let controller = Controller::bind(&root).unwrap();
    let first = Client::open(&root).unwrap();
    let second = Client::open(&root).unwrap();
    let changed = Policy {
        compiler: Access::Allowed,
        ..POLICY
    };
    let first_operation = Operation::Register {
        namespace: namespace('a').unwrap(),
        policy: POLICY,
    };
    let second_operation = Operation::Register {
        namespace: namespace('a').unwrap(),
        policy: changed,
    };
    let (served_first, served_second, first_reply, second_reply) = tokio::join!(
        controller.serve_one(),
        controller.serve_one(),
        first.request(first_operation),
        second.request(second_operation)
    );
    served_first.unwrap();
    served_second.unwrap();
    let first_reply = first_reply.unwrap();
    let second_reply = second_reply.unwrap();
    let winner = match (&first_reply, &second_reply) {
        (Reply::Applied, Reply::Conflict) => POLICY,
        (Reply::Conflict, Reply::Applied) => changed,
        _ => panic!("unexpected registration results: {first_reply:?}, {second_reply:?}"),
    };
    let Reply::Snapshot { namespaces, .. } =
        control_request(&controller, &first, Operation::Inspect {})
            .await
            .unwrap()
    else {
        panic!("missing registry")
    };
    assert_eq!(namespaces.len(), 1);
    assert_eq!(namespaces.get(&namespace('a').unwrap()), Some(&winner));
}

#[tokio::test]
async fn private_control_rejects_shared_and_symlinked_authority_roots() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap().join("shared");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        Controller::bind(&root),
        Err(wrix_service::control::Error::PrivateRoot)
    ));
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    let alias = directory.path().canonicalize().unwrap().join("alias");
    symlink(&root, &alias).unwrap();
    assert!(matches!(
        Controller::bind(&alias),
        Err(wrix_service::control::Error::PrivateRoot)
    ));
    assert!(matches!(
        Controller::bind(&alias.join("nested")),
        Err(wrix_service::control::Error::PrivateRoot)
    ));
    assert!(!root.join("nested").exists());
}

#[tokio::test]
async fn private_control_client_never_provisions_authority() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap().join("absent");
    assert!(Client::open(&root).is_err());
    assert!(!root.exists());
    std::fs::create_dir(&root).unwrap();
    assert!(Client::open(&root).is_err());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
}

#[tokio::test]
async fn concurrent_control_registrations_preserve_peer_policies_on_disk() -> TestResult {
    timeout(BUDGET, async {
        use std::os::unix::fs::MetadataExt;

        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?.join("controller");
        let controller = Controller::bind(&root)?;
        let socket = std::fs::metadata(root.join("control.sock"))?.ino();
        let key = std::fs::read(root.join("management.key"))?;
        let mut requests = JoinSet::new();
        let mut expected = std::collections::BTreeMap::new();
        for index in 0..32 {
            let namespace = Namespace::try_from(format!("{index:064x}"))?;
            let policy = Policy {
                compiler: if index % 2 == 0 {
                    Access::Allowed
                } else {
                    Access::Denied
                },
                ..POLICY
            };
            expected.insert(namespace.clone(), policy);
            let client = Client::open(&root)?;
            requests.spawn(async move {
                client
                    .request(Operation::Register { namespace, policy })
                    .await
            });
        }
        let serving = async {
            for _ in 0..32 {
                controller.serve_one().await?;
            }
            Ok::<(), TestError>(())
        };
        let receiving = async {
            while let Some(result) = requests.join_next().await {
                assert_eq!(result??, Reply::Applied);
            }
            Ok::<(), TestError>(())
        };
        let (served, received) = tokio::join!(serving, receiving);
        served?;
        received?;
        assert_eq!(std::fs::metadata(root.join("control.sock"))?.ino(), socket);
        assert_eq!(std::fs::read(root.join("management.key"))?, key);
        drop(controller);
        std::fs::remove_file(root.join("control.sock"))?;
        let controller = Controller::bind(&root)?;
        let client = Client::open(&root)?;
        assert_eq!(
            control_request(&controller, &client, Operation::Inspect {}).await?,
            Reply::Snapshot {
                version: 2,
                namespaces: expected
            }
        );
        Ok::<(), TestError>(())
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn registration_commit_failure_does_not_grant_or_erase_peer_policy() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    let client = Client::open(&root)?;
    let peer = namespace('a')?;
    assert_eq!(
        control_request(
            &controller,
            &client,
            Operation::Register {
                namespace: peer.clone(),
                policy: POLICY
            }
        )
        .await?,
        Reply::Applied
    );
    let before = std::fs::read(root.join("namespaces.json"))?;
    std::fs::rename(root.join("namespaces.json"), root.join("saved.json"))?;
    std::fs::create_dir(root.join("namespaces.json"))?;
    let entries_before = std::fs::read_dir(&root)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    let denied = namespace('b')?;
    let (served, reply) = tokio::join!(
        controller.serve_one(),
        client.request(Operation::Register {
            namespace: denied.clone(),
            policy: POLICY
        })
    );
    assert!(served.is_err());
    assert!(reply.is_err());
    // An unreadable durable registry is unavailable, not permission to replay
    // a formerly cached grant. Restoring the original file restores peer policy.
    assert!(controller.authorize(&peer, Permission::Sql).is_err());
    assert!(controller.authorize(&denied, Permission::Sql).is_err());
    assert_eq!(std::fs::read(root.join("saved.json"))?, before);
    let entries_after = std::fs::read_dir(&root)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    let mut expected = entries_before;
    expected.insert(format!("namespace-{}.lock", denied.as_str()).into());
    assert_eq!(entries_after, expected);
    std::fs::remove_dir(root.join("namespaces.json"))?;
    std::fs::rename(root.join("saved.json"), root.join("namespaces.json"))?;
    assert!(controller.authorize(&peer, Permission::Sql).is_ok());
    assert!(matches!(
        controller.authorize(&denied, Permission::Sql),
        Err(wrix_service::control::Error::UnknownNamespace)
    ));
    Ok(())
}

#[tokio::test]
async fn malformed_persisted_registration_fails_before_socket_or_credential_creation() -> TestResult
{
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    std::fs::create_dir(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let entry = serde_json::json!({"namespace": namespace('a')?, "policy": POLICY});
    for bytes in [
        b"{".to_vec(),
        serde_json::to_vec(&serde_json::json!({"version": 1, "entries": [entry.clone(), entry]}))?,
        serde_json::to_vec(&serde_json::json!({"version": 999, "entries": []}))?,
        serde_json::to_vec(
            &serde_json::json!({"version": 1, "entries": [{"namespace": "../peer", "policy": POLICY}]}),
        )?,
    ] {
        std::fs::write(root.join("namespaces.json"), &bytes)?;
        std::fs::set_permissions(
            root.join("namespaces.json"),
            std::fs::Permissions::from_mode(0o600),
        )?;
        assert!(Controller::bind(&root).is_err());
        assert_eq!(std::fs::read(root.join("namespaces.json"))?, bytes);
        assert!(!root.join("control.sock").exists());
        assert!(!root.join("management.key").exists());
    }
    Ok(())
}

#[tokio::test]
async fn namespace_retirement_is_durable_without_changing_peer_authority() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    let client = Client::open(&root)?;
    for digit in ['a', 'b'] {
        assert_eq!(
            control_request(
                &controller,
                &client,
                Operation::Register {
                    namespace: namespace(digit)?,
                    policy: Policy {
                        dolt: if digit == 'a' {
                            Access::Denied
                        } else {
                            Access::Allowed
                        },
                        ..POLICY
                    }
                }
            )
            .await?,
            Reply::Applied
        );
    }
    assert_eq!(
        control_request(
            &controller,
            &client,
            Operation::Retire {
                namespace: namespace('a')?
            }
        )
        .await?,
        Reply::Applied
    );
    drop(controller);
    std::fs::remove_file(root.join("control.sock"))?;
    let controller = Controller::bind(&root)?;
    assert!(matches!(
        controller.authorize(&namespace('a')?, Permission::Sql),
        Err(wrix_service::control::Error::UnknownNamespace)
    ));
    assert!(
        controller
            .authorize(&namespace('b')?, Permission::Sql)
            .is_ok()
    );
    Ok(())
}

#[tokio::test]
async fn controller_host_retains_live_role_routes_during_concurrent_registration() -> TestResult {
    timeout(BUDGET, async {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?.join("controller");
        let controller = Controller::bind(&root)?;
        let host =
            wrix_service::control::serving::Host::bind(controller, NonZeroUsize::new(8).unwrap())
                .await?;
        let peers = Peers::start(b'P').await?;
        for role in Role::ALL {
            host.selection()
                .select(peers.generation.confirm_ready(role))?;
        }
        let [a, b, c, d] = Role::ALL.map(|role| host.address(role));
        let addresses = [a?, b?, c?, d?];
        let leases = std::fs::read(root.join("role-endpoints.json"))?;
        let (stop, shutdown) = oneshot::channel();
        let task = tokio::spawn(host.serve(async { shutdown.await.unwrap() }));
        let mut streams = Vec::new();
        for (index, address) in addresses.iter().enumerate() {
            streams.push(connect(*address, [b'P', u8::try_from(index)?]).await?);
        }
        let first = Client::open(&root)?;
        let second = Client::open(&root)?;
        let (first, second) = tokio::join!(
            first.request(Operation::Register {
                namespace: namespace('a')?,
                policy: POLICY
            }),
            second.request(Operation::Register {
                namespace: namespace('b')?,
                policy: POLICY
            })
        );
        assert_eq!(first?, Reply::Applied);
        assert_eq!(second?, Reply::Applied);
        let client = Client::open(&root)?;
        assert_eq!(
            client
                .request(Operation::Register {
                    namespace: namespace('a')?,
                    policy: Policy {
                        dolt: Access::Denied,
                        ..POLICY
                    },
                })
                .await?,
            Reply::Conflict
        );
        for stream in &mut streams {
            echo(stream, b"established stream survives registration").await?;
        }
        for (index, address) in addresses.iter().enumerate() {
            let mut stream = connect(*address, [b'P', u8::try_from(index)?]).await?;
            echo(&mut stream, b"same listener still serves new connections").await?;
        }
        assert_eq!(std::fs::read(root.join("role-endpoints.json"))?, leases);
        assert_eq!(
            client.request(Operation::Inspect {}).await?,
            Reply::Snapshot {
                version: 2,
                namespaces: [(namespace('a')?, POLICY), (namespace('b')?, POLICY)].into(),
            }
        );
        stop.send(()).unwrap();
        task.await??;
        assert!(!root.join("control.sock").exists());
        for stream in &mut streams {
            assert_eq!(stream.read(&mut [0]).await?, 0);
        }
        Ok::<(), TestError>(())
    })
    .await?
}

#[tokio::test]
async fn controller_host_restart_keeps_policy_but_not_route_readiness() -> TestResult {
    timeout(BUDGET, async {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?.join("controller");
        let controller = Controller::bind(&root)?;
        let selected = controller.bootstrap_selection(
            wrix_sandbox::image::ImageRef::parse("service:blue")?,
            wrix_sandbox::image::Digest::parse(&format!("sha256:{}", "a".repeat(64)))?,
        )?;
        let host =
            wrix_service::control::serving::Host::bind(controller, NonZeroUsize::MIN).await?;
        let peers = Peers::start(b'P').await?;
        host.selection()
            .select(peers.generation.confirm_ready(Role::Sql))?;
        let address = host.address(Role::Sql)?;
        let (stop, shutdown) = oneshot::channel();
        let task = tokio::spawn(host.serve(async { shutdown.await.unwrap() }));
        let client = Client::open(&root)?;
        assert_eq!(
            client
                .request(Operation::Register {
                    namespace: namespace('a')?,
                    policy: POLICY,
                })
                .await?,
            Reply::Applied
        );
        let mut stream = connect(address, [b'P', 0]).await?;
        echo(&mut stream, b"known live route before restart").await?;
        drop(stream);
        stop.send(()).unwrap();
        task.await??;

        let controller = Controller::bind(&root)?;
        assert_eq!(controller.recorded_selection()?, Some(selected.clone()));
        assert_eq!(
            controller.bootstrap_selection(
                wrix_sandbox::image::ImageRef::parse("service:green")?,
                wrix_sandbox::image::Digest::parse(&format!("sha256:{}", "b".repeat(64)))?,
            )?,
            selected
        );
        let host =
            wrix_service::control::serving::Host::bind(controller, NonZeroUsize::MIN).await?;
        assert_eq!(host.address(Role::Sql)?, address);
        let [a, b, c, d] = Role::ALL.map(|role| host.address(role));
        let addresses = [a?, b?, c?, d?];
        let (stop, shutdown) = oneshot::channel();
        let task = tokio::spawn(host.serve(async { shutdown.await.unwrap() }));
        for address in addresses {
            rejected(address).await?;
        }
        assert_eq!(
            client.request(Operation::Inspect {}).await?,
            Reply::Snapshot {
                version: 2,
                namespaces: [(namespace('a')?, POLICY)].into(),
            }
        );
        stop.send(()).unwrap();
        task.await??;
        Ok::<(), TestError>(())
    })
    .await?
}

#[tokio::test]
async fn controller_host_initialization_failure_releases_unstarted_control() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?.join("controller");
    let controller = Controller::bind(&root)?;
    assert!(matches!(
        wrix_service::control::serving::Host::bind(controller, NonZeroUsize::new(1025).unwrap())
            .await,
        Err(wrix_service::control::Error::Route(
            wrix_service::route::Error::CapacityExceeded
        ))
    ));
    assert!(!root.join("control.sock").exists());
    let controller = Controller::bind(&root)?;
    let host = wrix_service::control::serving::Host::bind(controller, NonZeroUsize::MIN).await?;
    host.serve(std::future::ready(())).await?;
    assert!(!root.join("control.sock").exists());
    Ok(())
}
