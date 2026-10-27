use super::test::{native_query, start_native};
use super::*;
use crate::lifecycle::{
    drain::{Command, Response},
    sql::{self, Gate},
};
use std::{fs, net::TcpListener, os::unix::fs::FileTypeExt, time::Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Socket {
    path: std::path::PathBuf,
    device: u64,
    inode: u64,
}
impl Socket {
    fn owned(path: &Path) -> Self {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert!(metadata.file_type().is_socket());
        Self {
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.dev() == self.device && metadata.ino() == self.inode => {
                fs::remove_file(&self.path).unwrap();
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            result => panic!("test-owned native socket changed identity: {result:?}"),
        }
    }
}

async fn packet(stream: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
    let mut header = [0; 4];
    stream.read_exact(&mut header).await.unwrap();
    let size = usize::from(header[0]) | usize::from(header[1]) << 8 | usize::from(header[2]) << 16;
    assert!(size < 65536);
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await.unwrap();
    bytes
}

async fn send(stream: &mut (impl AsyncWrite + Unpin), sequence: u8, bytes: &[u8]) {
    let size = u32::try_from(bytes.len()).unwrap().to_le_bytes();
    stream
        .write_all(&[size[0], size[1], size[2], sequence])
        .await
        .unwrap();
    stream.write_all(bytes).await.unwrap();
    stream.flush().await.unwrap();
}

async fn login<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S) -> S {
    assert_eq!(packet(&mut stream).await[0], 10);
    let mut auth = Vec::new();
    auth.extend_from_slice(&0x0008_8209_u32.to_le_bytes());
    auth.extend_from_slice(&0x0100_0000_u32.to_le_bytes());
    auth.push(45);
    auth.extend_from_slice(&[0; 23]);
    auth.extend_from_slice(b"idle\0\0controller\0mysql_native_password\0");
    send(&mut stream, 1, &auth).await;
    let mut response = packet(&mut stream).await;
    if response[0] == 0xfe {
        send(&mut stream, 3, &[]).await;
        response = packet(&mut stream).await;
    }
    assert_eq!(response[0], 0, "native authentication failed: {response:?}");
    stream
}

async fn ping(stream: &mut (impl AsyncRead + AsyncWrite + Unpin)) {
    send(stream, 0, &[14]).await;
    assert_eq!(packet(stream).await[0], 0);
}

async fn rejected(mut stream: impl AsyncRead + Unpin) {
    let mut bytes = [0; 4];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), stream.read(&mut bytes))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

async fn command(action: Command) -> Response {
    tokio::task::spawn_blocking(move || sql::dispatch(action))
        .await
        .unwrap()
        .unwrap()
}

/// Uses fixed private native ports; refuses existing listeners or management sockets.
#[tokio::test]
#[ignore = "requires writable /run/wrix/dolt, exclusive ports3306/13306 and absent native/drain sockets; actual Dolt only, no VM"]
async fn native_sql_sessions_and_broker_work_drain_across_both_public_transports() -> TestResult {
    let native_socket = Path::new("/run/wrix/dolt/native.sock");
    let control_socket = Path::new("/run/wrix/dolt/drain.sock");
    assert!(
        !native_socket.try_exists()? && !control_socket.try_exists()?,
        "refusing pre-existing private endpoints"
    );
    let public_port = TcpListener::bind(("0.0.0.0", 3306))?;
    let native_port = TcpListener::bind(("127.0.0.1", 13306))?;
    let root = tempfile::Builder::new().prefix("wg-").tempdir()?;
    let data = root.path();
    fs::create_dir(data.join("databases"))?;
    fs::create_dir(data.join("run"))?;
    let owner = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(data.join("owner.lock"))?;
    owner.lock_exclusive()?;
    super::super::private_directory(native_socket.parent().unwrap())?;
    initialize(data)?;
    let config = data.join("native.json");
    fs::write(
        &config,
        serde_json::to_vec(&configuration_value(data, false))?,
    )?;
    drop(native_port);
    let mut native = start_native(data, &config, native_socket).await?;
    let _native_socket = Socket::owned(native_socket);
    assert_eq!(
        native_query(data, 13306, "SELECT @@GLOBAL.secure_file_priv AS hardened")?["rows"][0]["hardened"],
        "/dev/null"
    );
    native_query(
        data,
        13306,
        "CREATE USER 'idle'@'localhost' IDENTIFIED BY ''; GRANT SELECT ON controller.* TO 'idle'@'localhost'",
    )?;
    let namespace = wrix_core::path::WorkspaceHash::parse(&"c".repeat(64))?;
    let database = super::super::database_name(&namespace);
    let namespace_data = data.join("namespaces").join(namespace.as_str());
    fs::create_dir_all(namespace_data.join("native-remote"))?;
    File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(namespace_data.join("sync.lock"))?;
    let credentials = super::super::Credentials {
        database: database.clone(),
        user: namespace.as_str()[..32].into(),
        password: "d".repeat(64),
    };
    super::super::atomic_json(
        &namespace_data.join("access.json"),
        &super::super::broker::Access {
            credentials: credentials.clone(),
            branch: wrix_core::git::Branch::parse("beads")?,
            enabled: true,
        },
    )?;
    native_query(data, 13306, &format!("CREATE DATABASE `{database}`"))?;
    let remote = format!("file://{}", namespace_data.join("native-remote").display());
    let configured = super::super::broker::admin(
        data,
        &database,
        &format!(
            "CALL DOLT_REMOTE('add', 'origin', {})",
            super::super::literal(&remote)
        ),
    )?;
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    drop(public_port);
    let gate = Gate::default();
    assert!(gate.admit_broker().is_err());
    {
        let serving = async {
            tokio::try_join!(
                async { sql::serve(data, gate.clone()).await.map_err(Error::from) },
                super::super::broker::serve(data, false, gate.clone())
            )
        };
        tokio::pin!(serving);
        let operations = async {
            let socket = data.join("run/dolt.sock");
            let broker = data.join("run/sync.sock");
            while !control_socket.exists() || !socket.exists() || !broker.exists() {
                tokio::task::yield_now().await;
            }
            let _control_socket = Socket::owned(control_socket);
            rejected(tokio::net::TcpStream::connect(("127.0.0.1", 3306)).await?).await;
            rejected(tokio::net::UnixStream::connect(&socket).await?).await;
            rejected(tokio::net::UnixStream::connect(&broker).await?).await;
            assert_eq!(
                command(Command::Abort).await.status.phase,
                crate::lifecycle::drain::Phase::Serving
            );
            let mut tcp = login(tokio::net::TcpStream::connect(("127.0.0.1", 3306)).await?).await;
            let mut unix = login(tokio::net::UnixStream::connect(&socket).await?).await;
            let mut direct =
                login(tokio::net::TcpStream::connect(("127.0.0.1", 13306)).await?).await;
            let mut admitted = tokio::net::UnixStream::connect(&broker).await?;
            while command(Command::Status).await.status.background == 0 {
                tokio::task::yield_now().await;
            }
            let draining = command(Command::Begin).await;
            assert!(draining.status.requests >= 3 && draining.status.background == 1);
            assert!(!draining.status.drained());
            rejected(tokio::net::TcpStream::connect(("127.0.0.1", 3306)).await?).await;
            rejected(tokio::net::UnixStream::connect(&socket).await?).await;
            rejected(tokio::net::UnixStream::connect(&broker).await?).await;
            ping(&mut tcp).await;
            ping(&mut unix).await;
            let request = serde_json::to_vec(
                &serde_json::json!({ "namespace": namespace.as_str(), "password": credentials.password(), "operation": "push" }),
            )?;
            admitted.write_u32(u32::try_from(request.len())?).await?;
            admitted.write_all(&request).await?;
            let size = admitted.read_u32().await?;
            let mut reply = vec![0; size as usize];
            admitted.read_exact(&mut reply).await?;
            assert_eq!(
                serde_json::from_slice::<super::super::broker::Reply>(&reply)?,
                super::super::broker::Reply::Applied
            );
            assert!(namespace_data.join("native-remote/manifest").is_file());
            assert!(
                namespace_data
                    .join("beads-sync/beads/.beads/dolt-remote/manifest")
                    .is_file()
            );
            drop(admitted);
            send(&mut tcp, 0, &[1]).await;
            send(&mut unix, 0, &[1]).await;
            drop((tcp, unix));
            assert!(
                !command(Command::Quiesce).await.status.drained(),
                "direct native session must prevent false proxy-only completion"
            );
            ping(&mut direct).await;
            send(&mut direct, 0, &[1]).await;
            drop(direct);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !command(Command::Quiesce).await.status.drained() {
                assert!(Instant::now() < deadline);
            }
            let password = fs::read(data.join("admin.password"))?;
            fs::write(data.join("admin.password"), "e".repeat(64))?;
            assert!(
                tokio::task::spawn_blocking(|| sql::dispatch(Command::Quiesce))
                    .await?
                    .is_err(),
                "failed native authentication cannot attest quiescence"
            );
            fs::write(data.join("admin.password"), password)?;
            assert!(command(Command::Quiesce).await.status.drained());
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        tokio::select! {
            result = &mut serving => panic!("native serving stopped: {result:?}"),
            result = operations => result?,
        }
    }
    stop(&mut native).await?;
    assert!(native.wait().await?.success());
    drop(owner);
    Ok(())
}
