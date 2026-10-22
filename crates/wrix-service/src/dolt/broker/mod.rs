//! Namespace-authenticated data sync, not a host management or arbitrary SQL endpoint.

use super::{Credentials, Error, Sync};
use crate::control::Namespace;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    net::TcpStream,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use wrix_core::{git::Branch, path::WorkspaceHash};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Access {
    pub credentials: Credentials,
    #[serde(serialize_with = "super::serialize_branch")]
    pub branch: Branch,
    pub enabled: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    namespace: Namespace,
    password: String,
    operation: Sync,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Applied,
    Rejected,
    NonFastForward,
    Failed,
}

/// A tenant credential authorizes only these two operations in its recorded namespace.
pub fn request(
    endpoint: &Endpoint,
    credentials: &Credentials,
    operation: Sync,
) -> Result<Reply, Error> {
    let request = Request {
        namespace: Namespace::from(&super::namespace_from_database(credentials.database())?),
        password: credentials.password().to_owned(),
        operation,
    };
    let bytes = serde_json::to_vec(&request)?;
    let mut stream: Box<dyn Connection> = match endpoint {
        Endpoint::Unix { path } => {
            let stream = UnixStream::connect(path)?;
            stream.set_read_timeout(Some(Duration::from_secs(35)))?;
            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            Box::new(stream)
        }
        Endpoint::Tcp { host, port } => {
            let stream = TcpStream::connect_timeout(
                &std::net::SocketAddr::from((*host, port.get())),
                Duration::from_secs(5),
            )?;
            stream.set_read_timeout(Some(Duration::from_secs(35)))?;
            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            Box::new(stream)
        }
    };
    stream.write_all(
        &u32::try_from(bytes.len())
            .map_err(|_| Error::Result)?
            .to_be_bytes(),
    )?;
    stream.write_all(&bytes)?;
    let mut size = [0; 4];
    stream.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size);
    if size > 256 {
        return Err(Error::Result);
    }
    let mut bytes = vec![0; size as usize];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

trait Connection: Read + Write {}
impl<T: Read + Write> Connection for T {}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum Endpoint {
    Unix {
        path: PathBuf,
    },
    Tcp {
        host: std::net::Ipv4Addr,
        port: std::num::NonZeroU16,
    },
}

pub(crate) async fn serve(data: &Path, tcp: bool) -> Result<(), Error> {
    let socket = data.join("run/sync.sock");
    if socket.exists() {
        fs::remove_file(&socket)?;
    }
    let unix = tokio::net::UnixListener::bind(&socket)?;
    let listener = if tcp {
        Some(tokio::net::TcpListener::bind(("0.0.0.0", 3307)).await?)
    } else {
        None
    };
    let capacity = std::sync::Arc::new(tokio::sync::Semaphore::new(32));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            completed = tasks.join_next(), if !tasks.is_empty() => {
                match completed {
                    Some(Err(_) | Ok(Err(Error::Task))) => return Err(Error::Task),
                    Some(Ok(Err(error))) => tracing::warn!(%error, "sync request rejected"),
                    Some(Ok(Ok(()))) | None => (),
                }
            }
            connection = unix.accept() => {
                let (stream, _) = connection?;
                if let Ok(permit) = capacity.clone().try_acquire_owned() {
                    let data = data.to_owned();
                    tasks.spawn(async move { let _permit = permit; exchange(stream, &data).await });
                }
            }
            connection = async { match &listener { Some(listener) => listener.accept().await, None => std::future::pending().await } } => {
                let (stream, _) = connection?;
                if let Ok(permit) = capacity.clone().try_acquire_owned() {
                    let data = data.to_owned();
                    tasks.spawn(async move { let _permit = permit; exchange(stream, &data).await });
                }
            }
        }
    }
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    data: &Path,
) -> Result<(), Error> {
    let request: Request = tokio::time::timeout(Duration::from_secs(5), async {
        let size = stream.read_u32().await?;
        if size > 2048 {
            return Err(Error::Unauthorized);
        }
        let mut bytes = vec![0; size as usize];
        stream.read_exact(&mut bytes).await?;
        Ok::<_, Error>(serde_json::from_slice(&bytes)?)
    })
    .await
    .map_err(|_| Error::Unauthorized)??;
    let data = data.to_owned();
    let reply = tokio::task::spawn_blocking(move || apply(&data, &request))
        .await
        .map_err(|_| Error::Task)??;
    let bytes = serde_json::to_vec(&reply)?;
    tokio::time::timeout(Duration::from_secs(5), async {
        stream
            .write_u32(u32::try_from(bytes.len()).map_err(|_| Error::Result)?)
            .await?;
        stream.write_all(&bytes).await?;
        Ok::<_, Error>(())
    })
    .await
    .map_err(|_| Error::Result)??;
    Ok(())
}

fn apply(data: &Path, request: &Request) -> Result<Reply, Error> {
    let namespace = data.join("namespaces").join(request.namespace.as_str());
    super::check_directory(&namespace)?;
    let path = namespace.join("access.json");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= 4096 => (),
        Ok(_) => return Ok(Reply::Rejected),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Reply::Rejected),
        Err(error) => return Err(error.into()),
    }
    let access: Access = serde_json::from_slice(&fs::read(path)?)?;
    if !access.enabled
        || access.credentials.database()
            != super::database_name(
                &WorkspaceHash::parse(request.namespace.as_str())
                    .map_err(|_| Error::Unauthorized)?,
            )
        || !equal(
            access.credentials.password().as_bytes(),
            request.password.as_bytes(),
        )
    {
        return Ok(Reply::Rejected);
    }
    let lock = fs::File::options()
        .read(true)
        .write(true)
        .open(namespace.join("sync.lock"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return Ok(Reply::Failed),
        }
    }
    let current: Access = serde_json::from_slice(&fs::read(namespace.join("access.json"))?)?;
    if !current.enabled
        || current.branch != access.branch
        || current.credentials.database() != access.credentials.database()
        || current.credentials.user() != access.credentials.user()
        || !equal(
            current.credentials.password().as_bytes(),
            request.password.as_bytes(),
        )
    {
        return Ok(Reply::Rejected);
    }
    let remote = format!("file://{}", namespace.join("native-remote").display());
    let query = "SELECT url FROM dolt_remotes WHERE name = 'origin'";
    let output = admin(data, access.credentials.database(), query)?;
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    if !output.status.success() || rows["rows"][0]["url"] != remote {
        return Ok(Reply::Rejected);
    }
    if request.operation == Sync::Pull {
        super::snapshot::transfer(&namespace, &access.branch, false)?;
    }
    let query = match request.operation {
        Sync::Push => "CALL DOLT_PUSH('origin', active_branch())",
        Sync::Pull => "CALL DOLT_PULL('origin', active_branch())",
    };
    let output = admin(data, access.credentials.database(), query)?;
    if output.status.success() {
        if request.operation == Sync::Push {
            super::snapshot::transfer(&namespace, &access.branch, true)?;
        }
        return Ok(Reply::Applied);
    }
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_lowercase();
    if [
        "non-fast-forward",
        "non fast forward",
        "not a fast-forward",
        "cannot fast forward",
        "remote contains work",
        "fetch first",
        "behind",
        "out of date",
    ]
    .iter()
    .any(|needle| output.contains(needle))
    {
        Ok(Reply::NonFastForward)
    } else {
        Ok(Reply::Failed)
    }
}

pub(crate) fn admin(
    data: &Path,
    database: &str,
    query: &str,
) -> Result<std::process::Output, Error> {
    let password = fs::read_to_string(data.join("admin.password"))?;
    let mut command = Command::new("timeout");
    command
        .args([
            "--signal=KILL",
            "30",
            "dolt",
            "--host",
            "127.0.0.1",
            "--port",
            "3306",
            "--no-tls",
            "--password",
            &password,
            "--user",
            "wrixadmin",
        ])
        .env("DOLT_CLI_PASSWORD", password)
        .env_remove("DOLT_ROOT_HOST")
        .env_remove("DOLT_ROOT_PASSWORD")
        .current_dir(data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !database.is_empty() {
        command.args(["--use-db", database]);
    }
    command.args(["sql", "-r", "json"]);
    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .ok_or(Error::Result)?
        .write_all(query.as_bytes())?;
    Ok(child.wait_with_output()?)
}

fn equal(first: &[u8], second: &[u8]) -> bool {
    first.len() == second.len()
        && first
            .iter()
            .zip(second)
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}
