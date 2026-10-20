//! Bounded private component control; a disconnected caller never cancels or releases native work.

use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    time::Duration,
};

use super::{
    Error,
    wire::{self, Action, Observation, Reply, Request},
};
use crate::control::Namespace;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::{Instant, timeout, timeout_at},
};

const IO_BUDGET: Duration = Duration::from_secs(3);
const RETIRE_BUDGET: Duration = Duration::from_secs(10);

/// Requests never create a control endpoint or select an owner.
pub async fn request(
    root: &Path,
    namespace: &Namespace,
    action: Action,
) -> Result<Observation, Error> {
    timeout(IO_BUDGET, async {
        let directory = fs::symlink_metadata(root)?;
        let socket = fs::symlink_metadata(root.join(wire::SOCKET))?;
        if !directory.is_dir()
            || directory.mode() & 0o077 != 0
            || !socket.file_type().is_socket()
            || socket.uid() != directory.uid()
        {
            return Err(Error::Permissions);
        }
        let mut stream = UnixStream::connect(root.join(wire::SOCKET)).await?;
        if stream.peer_cred()?.uid() != directory.uid() {
            return Err(Error::Permissions);
        }
        let bytes = serde_json::to_vec(&Request {
            version: wire::VERSION,
            namespace: namespace.clone(),
            action,
        })?;
        stream.write_all(&bytes).await?;
        stream.shutdown().await?;
        let mut bytes = Vec::new();
        (&mut stream)
            .take(wire::LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > wire::LIMIT {
            return Err(Error::Incompatible);
        }
        let reply: Reply = serde_json::from_slice(&bytes)?;
        if reply.version != wire::VERSION || reply.observation == Observation::Rejected {
            return Err(Error::Incompatible);
        }
        Ok(reply.observation)
    })
    .await
    .map_err(|_| Error::Deadline)?
}

/// Completion requires the native child exit and inherited-epoch exclusion, not a quiet endpoint.
pub async fn retire(root: &Path, namespace: &Namespace) -> Result<(), Error> {
    let deadline = Instant::now() + RETIRE_BUDGET;
    let result = async {
        let mut observation = request(root, namespace, Action::Retire).await?;
        loop {
            match observation {
                Observation::Retired => return Ok(()),
                Observation::Retiring => (),
                Observation::Busy => return Err(Error::Busy),
                Observation::Active | Observation::Rejected => return Err(Error::Incompatible),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            observation = request(root, namespace, Action::Inspect).await?;
        }
    };
    timeout_at(deadline, result)
        .await
        .map_err(|_| Error::Deadline)?
        .map_err(|source| Error::Unconfirmed {
            source: Box::new(source),
        })
}
