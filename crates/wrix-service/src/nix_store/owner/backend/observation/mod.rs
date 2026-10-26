//! Passive recorded ownership and protection; no runtime or native readiness is inferred.
#[cfg(test)]
mod test;

use super::{Error, LEDGER, Record, State, check_directory, client};
use crate::deployment;
use std::{
    fs,
    io::{self, Write},
    path::Path,
};
use wrix_core::nix_store::pool::{Key, System};
use wrix_sandbox::image::Digest;

enum Phase {
    Starting,
    Created,
    Serving,
    Stopping,
    Released,
}
impl Phase {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Created => "created",
            Self::Serving => "serving",
            Self::Stopping => "stopping",
            Self::Released => "released",
        }
    }
}

pub struct Snapshot {
    phase: Phase,
    image: Digest,
    retained: usize,
}
impl Snapshot {
    pub fn write(&self, system: System, stdout: &mut impl Write) -> io::Result<()> {
        writeln!(
            stdout,
            "{}: recorded native owner {}; readiness not probed",
            system.as_str(),
            self.phase.as_str()
        )?;
        writeln!(
            stdout,
            "{}: recorded execution protection: {} retained; live readers/work not probed",
            system.as_str(),
            self.retained
        )?;
        writeln!(
            stdout,
            "{}: recorded owner image {}; compatibility not probed",
            system.as_str(),
            self.image.as_str()
        )
    }

    fn read(root: &Path, deployment: &deployment::Id, pool: &Key) -> Result<Self, Error> {
        check_directory(root)?;
        let record = Record::read(root, deployment, pool)?;
        let retained = client::retained(root, pool, &record.executions)?;
        let (phase, software) = match &record.state {
            State::Starting { attempt } => (Phase::Starting, &attempt.software),
            State::Created { target } => (Phase::Created, &target.attempt.software),
            State::Serving { connection } => (Phase::Serving, &connection.target.attempt.software),
            State::Stopping { connection } => {
                (Phase::Stopping, &connection.target.attempt.software)
            }
            State::Released { connection } => {
                (Phase::Released, &connection.target.attempt.software)
            }
        };
        Ok(Self {
            phase,
            image: software.digest.clone(),
            retained,
        })
    }
}

pub fn inspect(
    host: &wrix_state::host::Host,
    deployment: &deployment::Id,
    pool: &Key,
) -> Result<Option<Snapshot>, Error> {
    let root = host
        .roots()
        .state()
        .join("control/nix")
        .join(pool.repository.to_string())
        .join(pool.system.as_str());
    match fs::symlink_metadata(root.join(LEDGER)) {
        Ok(_) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    Snapshot::read(&root, deployment, pool).map(Some)
}
