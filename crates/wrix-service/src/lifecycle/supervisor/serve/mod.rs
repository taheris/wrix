//! Private component supervision with explicit retirement and inherited native-work exclusion.

use std::{
    fs,
    future::{Future, pending},
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::Path,
    process::{Command as NativeCommand, ExitStatus, Stdio},
    time::Duration,
};

use super::{
    Error, Role,
    hold::Held,
    state::{Intent, Owner},
    wire::{self, Action, Observation, Reply, Request},
};
use crate::control::Namespace;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    process::{Child, Command},
    sync::{mpsc, oneshot},
};

enum Phase {
    Active { child: Child },
    Retiring { child: Child },
    Draining,
    Retired(Held),
}

impl Phase {
    const fn observation(&self) -> Observation {
        match self {
            Self::Active { .. } => Observation::Active,
            Self::Retiring { .. } | Self::Draining => Observation::Retiring,
            Self::Retired(_) => Observation::Retired,
        }
    }

    async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        match self {
            Self::Active { child, .. } | Self::Retiring { child, .. } => child.wait().await,
            Self::Draining | Self::Retired(_) => pending().await,
        }
    }
}

struct Message {
    request: Request,
    reply: oneshot::Sender<Observation>,
}

enum Event {
    First(std::io::Result<ExitStatus>),
    Second(std::io::Result<ExitStatus>),
    Connection(std::io::Result<(UnixStream, tokio::net::unix::SocketAddr)>),
    Request(Option<Message>),
    Probe,
    Stop,
}

/// Shutdown is host/runtime authority, deliberately absent from the namespace control protocol.
pub async fn run(
    root: &Path,
    namespace: Namespace,
    dolt_socket: &Path,
    first_recipe: &str,
    second_recipe: &str,
    shutdown: impl Future<Output = ()>,
) -> Result<(), Error> {
    let mut owner = Owner::open(root, namespace)?;
    let held = Held::acquire(root)?.ok_or(Error::Busy)?;
    let socket = root.join(wire::SOCKET);
    match fs::symlink_metadata(&socket) {
        Ok(metadata) if metadata.file_type().is_socket() && metadata.uid() == owner.uid() => {
            fs::remove_file(&socket)?;
        }
        Ok(_) => return Err(Error::Permissions),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    if owner.intent() == Intent::Active {
        remove_stale_socket(dolt_socket, owner.uid())?;
    }
    owner.begin()?;
    let mut first = spawn(first_recipe)?;
    let mut phase = match owner.intent() {
        Intent::Active => match held.spawn(root, second_recipe) {
            Ok(child) => Phase::Active { child },
            Err(error) => {
                return complete(
                    Err(error),
                    cleanup(&mut first, Phase::Draining, &mut owner, root, &socket).await,
                );
            }
        },
        Intent::Retired => Phase::Retired(held),
    };
    let (send, mut receive) = mpsc::channel(16);
    let capacity = std::sync::Arc::new(tokio::sync::Semaphore::new(16));
    let mut probe = tokio::time::interval(Duration::from_millis(50));
    tokio::pin!(shutdown);
    let result = loop {
        let event = tokio::select! {
            status = first.wait() => Event::First(status),
            status = phase.wait() => Event::Second(status),
            connection = listener.accept() => Event::Connection(connection),
            message = receive.recv() => Event::Request(message),
            _ = probe.tick() => Event::Probe,
            () = &mut shutdown => Event::Stop,
        };
        match event {
            Event::First(status) => {
                break match status {
                    Ok(status) => Err(Error::Exited {
                        role: Role::Celler,
                        code: status.code(),
                    }),
                    Err(error) => Err(error.into()),
                };
            }
            Event::Second(status) => {
                let status = match status {
                    Ok(status) => status,
                    Err(error) => break Err(error.into()),
                };
                if matches!(phase, Phase::Active { .. }) {
                    break Err(Error::Exited {
                        role: Role::Dolt,
                        code: status.code(),
                    });
                }
                phase = Phase::Draining;
            }
            Event::Connection(connection) => {
                let (stream, _) = match connection {
                    Ok(connection) => connection,
                    Err(error) => break Err(error.into()),
                };
                let permit = match capacity.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(error) => {
                        tracing::warn!(%error, "component control capacity exhausted");
                        continue;
                    }
                };
                let send = send.clone();
                let uid = owner.uid();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = connection_request(stream, uid, send).await {
                        tracing::warn!(%error, "component control request rejected");
                    }
                });
            }
            Event::Request(Some(message)) => {
                let action = message.request.action;
                let (next, observation) = if message.request.version != wire::VERSION
                    || &message.request.namespace != owner.namespace()
                {
                    (phase, Observation::Rejected)
                } else {
                    let (next, result) =
                        apply(&mut owner, phase, action, root, dolt_socket, second_recipe);
                    let observation = match result {
                        Ok(observation) => observation,
                        Err(error) => {
                            tracing::warn!(%error, ?action, "component transition remains unconfirmed");
                            Observation::Busy
                        }
                    };
                    (next, observation)
                };
                phase = next;
                if message.reply.send(observation).is_err() {
                    tracing::debug!(
                        ?action,
                        "component caller disconnected; transition is retained"
                    );
                }
            }
            Event::Request(None) => break Err(Error::Disconnected),
            Event::Probe => {
                if matches!(phase, Phase::Draining) {
                    match Held::acquire(root) {
                        Ok(Some(held)) => phase = Phase::Retired(held),
                        Ok(None) => (),
                        Err(error) => break Err(error),
                    }
                }
            }
            Event::Stop => break Ok(()),
        }
    };
    complete(
        result,
        cleanup(&mut first, phase, &mut owner, root, &socket).await,
    )
}

fn complete(result: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (result, cleanup) {
        (Err(source), Err(cleanup)) => Err(Error::Cleanup {
            source: Box::new(source),
            cleanup: Box::new(cleanup),
        }),
        (result, Ok(())) => result,
        (Ok(()), cleanup) => cleanup,
    }
}

async fn cleanup(
    first: &mut Child,
    phase: Phase,
    owner: &mut Owner,
    root: &Path,
    socket: &Path,
) -> Result<(), Error> {
    let (first, held) = tokio::join!(stop_child(first), stop_phase(phase, root));
    first?;
    let held = held?;
    owner.finish()?;
    fs::remove_file(socket)?;
    drop(held);
    Ok(())
}

async fn stop_child(child: &mut Child) -> Result<(), Error> {
    terminate(child)?;
    child.wait().await?;
    Ok(())
}

fn apply(
    owner: &mut Owner,
    phase: Phase,
    action: Action,
    root: &Path,
    socket: &Path,
    recipe: &str,
) -> (Phase, Result<Observation, Error>) {
    match (action, phase) {
        (Action::Retire, Phase::Active { child }) => {
            let result = owner
                .intend(Intent::Retired)
                .and_then(|()| terminate(&child));
            match result {
                Ok(()) => (Phase::Retiring { child }, Ok(Observation::Retiring)),
                Err(error) => (Phase::Active { child }, Err(error)),
            }
        }
        (Action::Resume, Phase::Retired(held)) => {
            if let Err(error) =
                remove_stale_socket(socket, owner.uid()).and_then(|()| owner.intend(Intent::Active))
            {
                return (Phase::Retired(held), Err(error));
            }
            match held.spawn(root, recipe) {
                Ok(child) => (Phase::Active { child }, Ok(Observation::Active)),
                Err(error) => (Phase::Draining, Err(error)),
            }
        }
        (Action::Resume, phase @ (Phase::Retiring { .. } | Phase::Draining)) => {
            (phase, Ok(Observation::Busy))
        }
        (Action::Inspect | Action::Retire | Action::Resume, phase) => {
            let observation = phase.observation();
            (phase, Ok(observation))
        }
    }
}

fn remove_stale_socket(socket: &Path, uid: u32) -> Result<(), Error> {
    match fs::symlink_metadata(socket) {
        Ok(metadata)
            if metadata.file_type().is_socket()
                && metadata.uid() == uid
                && metadata.nlink() == 1 =>
        {
            Ok(fs::remove_file(socket)?)
        }
        Ok(_) => Err(Error::Permissions),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn spawn(recipe: &str) -> Result<Child, Error> {
    Ok(Command::new("sh")
        .args(["-c", recipe])
        .stdin(Stdio::null())
        .kill_on_drop(false)
        .spawn()?)
}

fn terminate(child: &Child) -> Result<(), Error> {
    if let Some(pid) = child.id() {
        let output = NativeCommand::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(Error::Signal);
        }
    }
    Ok(())
}

async fn stop_phase(phase: Phase, root: &Path) -> Result<Held, Error> {
    match phase {
        Phase::Active { mut child } | Phase::Retiring { mut child } => {
            terminate(&child)?;
            child.wait().await?;
        }
        Phase::Retired(held) => return Ok(held),
        Phase::Draining => (),
    }
    Held::acquire(root)?.ok_or(Error::Unclean)
}

async fn connection_request(
    mut stream: UnixStream,
    uid: u32,
    send: mpsc::Sender<Message>,
) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(3), async {
        if stream.peer_cred()?.uid() != uid {
            return Err(Error::Permissions);
        }
        let mut bytes = Vec::new();
        (&mut stream)
            .take(wire::LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > wire::LIMIT {
            return Err(Error::Incompatible);
        }
        let request: Request = serde_json::from_slice(&bytes)?;
        let (reply, receive) = oneshot::channel();
        send.send(Message { request, reply })
            .await
            .map_err(|_| Error::Disconnected)?;
        let observation = receive.await.map_err(|_| Error::Disconnected)?;
        stream
            .write_all(&serde_json::to_vec(&Reply {
                version: wire::VERSION,
                observation,
            })?)
            .await?;
        stream.shutdown().await?;
        Ok(())
    })
    .await
    .map_err(|_| Error::Deadline)?
}

#[cfg(all(test, target_os = "linux"))]
mod test;
