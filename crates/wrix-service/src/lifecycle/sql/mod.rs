//! Native SQL admission counts whole sessions and observes native sessions independently before release.

use crate::lifecycle::drain::{Command, Failure, Phase, Response, Status};
use serde::Deserialize;
use std::{
    fs, io,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UnixListener},
    task::JoinSet,
};

const CONTROL: &str = "/run/wrix/dolt/drain.sock";
struct State {
    closed: bool,
    sessions: usize,
    broker: usize,
}
impl Default for State {
    fn default() -> Self {
        Self {
            closed: true,
            sessions: 0,
            broker: 0,
        }
    }
}
#[derive(Clone, Default)]
pub struct Gate(Arc<Mutex<State>>);

pub struct Lease {
    gate: Gate,
    broker: bool,
}
impl Drop for Lease {
    fn drop(&mut self) {
        match self.gate.0.lock() {
            Ok(mut state) => {
                if self.broker {
                    state.broker -= 1;
                } else {
                    state.sessions -= 1;
                }
            }
            Err(error) => tracing::error!(%error, "SQL native work lease remains uncertain"),
        }
    }
}
impl Gate {
    /// Every broker request retains this lease through native work and final result delivery.
    pub fn admit_broker(&self) -> Result<Lease, io::Error> {
        self.admit(true)
    }
    fn admit(&self, broker: bool) -> Result<Lease, io::Error> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| io::Error::other("SQL routing state lock poisoned"))?;
        if state.closed {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "SQL admission is closed for native maintenance",
            ));
        }
        if broker {
            state.broker = state
                .broker
                .checked_add(1)
                .ok_or_else(|| io::Error::other("SQL work capacity overflow"))?;
        } else {
            state.sessions = state
                .sessions
                .checked_add(1)
                .ok_or_else(|| io::Error::other("SQL session capacity overflow"))?;
        }
        Ok(Lease {
            gate: self.clone(),
            broker,
        })
    }
    async fn command(&self, data: &Path, command: Command) -> Response {
        let observation = (|| {
            let mut state = self
                .0
                .lock()
                .map_err(|_| io::Error::other("SQL routing state lock poisoned"))?;
            match command {
                Command::Begin => state.closed = true,
                Command::Abort => state.closed = false,
                Command::Release => {
                    return Err(io::Error::other(
                        "SQL release is the supervised native child zero exit plus original ownership lock, not a transport reply",
                    ));
                }
                Command::Status | Command::Quiesce => (),
            }
            Ok((state.closed, state.sessions, state.broker))
        })();
        let (closed, sessions, broker) = match observation {
            Ok(observation) => observation,
            Err(error) => {
                tracing::error!(%error, "native SQL drain request failed");
                return Response {
                    accepted: false,
                    status: Status {
                        phase: Phase::Draining,
                        requests: 1,
                        workflows: 0,
                        background: 1,
                        failed: true,
                    },
                };
            }
        };
        if !closed {
            return Response {
                accepted: true,
                status: Status {
                    phase: Phase::Serving,
                    requests: sessions,
                    workflows: 0,
                    background: broker,
                    failed: false,
                },
            };
        }
        let root = data.to_owned();
        let native = tokio::task::spawn_blocking(move || native_sessions(&root)).await;
        match native {
            Ok(Ok(native)) => Response {
                accepted: true,
                status: Status {
                    phase: Phase::Draining,
                    requests: sessions.saturating_add(native),
                    workflows: 0,
                    background: broker,
                    failed: false,
                },
            },
            failed => {
                tracing::warn!(?failed, "native SQL session completion is unconfirmed");
                Response {
                    accepted: false,
                    status: Status {
                        phase: Phase::Draining,
                        requests: sessions,
                        workflows: 0,
                        background: broker,
                        failed: true,
                    },
                }
            }
        }
    }
}

fn native_sessions(data: &Path) -> Result<usize, io::Error> {
    use std::process::Command;
    let password = fs::read_to_string(data.join("admin.password"))?;
    if password.len() != 64 || !password.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other(
            "native SQL observer credential is invalid",
        ));
    }
    let output = Command::new("timeout")
        .args([
            "--signal=TERM",
            "3",
            "dolt",
            "--host",
            "127.0.0.1",
            "--port",
            "13306",
            "--no-tls",
            "--user",
            "wrixadmin",
            "--use-db",
            "controller",
            "sql",
            "-r",
            "json",
            "-q",
            "SELECT ID FROM information_schema.processlist WHERE ID <> CONNECTION_ID()",
        ])
        .env("DOLT_CLI_PASSWORD", password)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            "authenticated native SQL session census failed",
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    value
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| io::Error::other("native SQL session census response is invalid"))
}

fn clear_socket(path: &Path) -> Result<(), io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_socket()
                && metadata.nlink() == 1
                && metadata.uid() == rustix::process::geteuid().as_raw() =>
        {
            match std::os::unix::net::UnixStream::connect(path) {
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    fs::remove_file(path)
                }
                Err(error) => Err(error),
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "live SQL endpoint must not be removed",
                )),
            }
        }
        Ok(_) => Err(io::Error::other(
            "native SQL endpoint is not an owned original socket",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The backend supervisor holds its original data ownership inode before binding either plane.
/// Dolt itself listens only on private native 127.0.0.1:13306 and a separate private socket.
pub async fn serve(data: &Path, gate: Gate) -> Result<(), io::Error> {
    crate::dolt::private_directory(Path::new("/run/wrix/dolt")).map_err(io::Error::other)?;
    let socket = data.join("run/dolt.sock");
    clear_socket(&socket)?;
    clear_socket(Path::new(CONTROL))?;
    let unix = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let tcp = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, 3306)).await?;
    let control = UnixListener::bind(CONTROL)?;
    fs::set_permissions(CONTROL, fs::Permissions::from_mode(0o600))?;
    serve_on(data, gate, tcp, unix, control).await
}

/// Serve already-owned descriptors; the embedding supervisor retains the original data lock.
/// Addresses and data authority are never selected by SQL clients or control messages.
pub async fn serve_on(
    data: &Path,
    gate: Gate,
    tcp: TcpListener,
    unix: UnixListener,
    control: UnixListener,
) -> Result<(), io::Error> {
    let capacity = Arc::new(tokio::sync::Semaphore::new(1024));
    let control_capacity = Arc::new(tokio::sync::Semaphore::new(32));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accepted = tcp.accept() => {
                let (mut incoming, _) = accepted?;
                let Ok(lease) = gate.admit(false) else { continue; };
                let Ok(permit) = capacity.clone().try_acquire_owned() else { continue; };
                tasks.spawn(async move {
                    let (_lease, _permit) = (lease, permit);
                    if let Err(error) = async {
                        let mut native = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, 13306)).await?;
                        tokio::io::copy_bidirectional(&mut incoming, &mut native).await?;
                        Ok::<_, io::Error>(())
                    }.await { tracing::debug!(%error, "native SQL session ended"); }
                });
            }
            accepted = unix.accept() => {
                let (mut incoming, _) = accepted?;
                let Ok(lease) = gate.admit(false) else { continue; };
                let Ok(permit) = capacity.clone().try_acquire_owned() else { continue; };
                tasks.spawn(async move {
                    let (_lease, _permit) = (lease, permit);
                    if let Err(error) = async {
                        let mut native = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, 13306)).await?;
                        tokio::io::copy_bidirectional(&mut incoming, &mut native).await?;
                        Ok::<_, io::Error>(())
                    }.await { tracing::debug!(%error, "native SQL Unix session ended"); }
                });
            }
            accepted = control.accept() => {
                let (stream, _) = accepted?;
                let Ok(permit) = control_capacity.clone().try_acquire_owned() else { continue; };
                let gate = gate.clone(); let data = data.to_owned();
                tasks.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = control_request(stream, gate, data).await { tracing::warn!(%error, "native SQL control request failed"); }
                });
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(error)) = result { return Err(io::Error::other(error)); }
            }
        }
    }
}

async fn control_request(
    stream: tokio::net::UnixStream,
    gate: Gate,
    data: PathBuf,
) -> Result<(), io::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        command: Wire,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Wire {
        Begin,
        Status,
        Quiesce,
        Abort,
        Release,
    }
    if stream.peer_cred()?.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private SQL control peer is not the native owner",
        ));
    }
    let mut stream = BufReader::new(stream);
    let mut bytes = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        (&mut stream).take(4097).read_until(b'\n', &mut bytes),
    )
    .await
    .map_err(io::Error::other)??;
    if bytes.len() > 4096 {
        return Err(io::Error::other("native SQL drain request is oversized"));
    }
    let request: Request = serde_json::from_slice(&bytes)?;
    let command = match request.command {
        Wire::Begin => Command::Begin,
        Wire::Status => Command::Status,
        Wire::Quiesce => Command::Quiesce,
        Wire::Abort => Command::Abort,
        Wire::Release => Command::Release,
    };
    let response = gate.command(&data, command).await;
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    stream.get_mut().write_all(&bytes).await
}

pub fn dispatch(command: Command) -> Result<Response, Failure> {
    let mut stream = std::os::unix::net::UnixStream::connect(CONTROL)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    super::drain::exchange(&mut stream, command)
}

#[cfg(test)]
mod test {
    use super::*;
    #[tokio::test]
    async fn sql_gate_closes_admission_atomically_without_closing_existing_broker_work() {
        let root = tempfile::tempdir().unwrap();
        let gate = Gate::default();
        gate.command(root.path(), Command::Abort).await;
        let broker = gate.admit_broker().unwrap();
        {
            gate.0.lock().unwrap().closed = true;
        }
        assert!(gate.admit_broker().is_err());
        assert_eq!(gate.0.lock().unwrap().broker, 1);
        drop(broker);
        assert_eq!(gate.0.lock().unwrap().broker, 0);
        let response = gate.command(root.path(), Command::Abort).await;
        assert_eq!(response.status.phase, Phase::Serving);
        assert!(gate.admit_broker().is_ok());
    }
}
