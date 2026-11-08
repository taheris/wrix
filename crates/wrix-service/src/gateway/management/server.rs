//! Authenticated server handling over the shared bounded management frames.

use super::{Error, Secret, State, policy::Policy};
use tokio::{io::AsyncReadExt, net::TcpStream, time::timeout};
use wrix_service::gateway::management::wire::{BUDGET, Envelope, Operation, Reply, read, write};
pub use wrix_service::gateway::management::{Attachment, Client, Status, VERSION};

pub(super) async fn serve(mut stream: TcpStream, key: &Secret, state: &State) -> Result<(), Error> {
    let envelope: Envelope = timeout(BUDGET, read(&mut stream))
        .await
        .map_err(std::io::Error::other)??;
    if envelope.version != VERSION || !key.matches(&envelope.key) {
        write(&mut stream, &Reply::Denied).await?;
        return Err(Error::Denied);
    }
    match envelope.operation {
        Operation::Status => write(&mut stream, &Reply::Status(state.status()?)).await,
        Operation::Drain { closed } => {
            state.table.lock().map_err(|_| Error::Poisoned)?.draining = closed;
            write(&mut stream, &Reply::Status(state.status()?)).await
        }
        Operation::Attach { credential, policy } => {
            hold(&mut stream, state.attach(credential, policy, false, true)?).await
        }
        Operation::Prepare { credential, policy } => {
            hold(&mut stream, state.attach(credential, policy, false, false)?).await
        }
        Operation::Probe { credential } => {
            hold(
                &mut stream,
                state.attach(credential, probe_policy()?, true, true)?,
            )
            .await
        }
    }
}

fn probe_policy() -> Result<Policy, Error> {
    Policy::new(vec![super::policy::Rule::new(
        super::policy::Host::parse("probe.wrix.invalid")?,
        vec![std::num::NonZeroU16::new(80).ok_or(Error::Invalid)?],
    )?])
}

async fn hold(stream: &mut TcpStream, revoker: super::Revoker) -> Result<(), Error> {
    write(stream, &Reply::Attached).await?;
    let mut byte = [0];
    loop {
        match stream.read(&mut byte).await? {
            0 => return Ok(()),
            1 if byte[0] == 0 => {
                drop(revoker);
                return write(stream, &Reply::Detached).await;
            }
            1 if byte[0] == 1 => {
                revoker.activate()?;
                write(stream, &Reply::Activated).await?;
            }
            _ => return Err(Error::Invalid),
        }
    }
}
