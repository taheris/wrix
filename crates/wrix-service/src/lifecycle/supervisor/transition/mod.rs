//! Host-private uncertainty fence; a lost caller never authorizes another component mutation.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use super::{Action, Error, Observation};
use crate::{control::Namespace, lifecycle::coordination};
use serde::{Deserialize, Serialize};
use wrix_core::path::WorkspaceHash;

const FILE: &str = "component-transition.json";
const LIMIT: u64 = 4096;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Retire,
    Resume,
}

impl Change {
    pub const fn action(self) -> Action {
        match self {
            Self::Retire => Action::Retire,
            Self::Resume => Action::Resume,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    namespace: Namespace,
    change: Change,
}

pub struct Pending {
    root: PathBuf,
    change: Change,
}

pub fn ensure_clear(root: &Path, workspace: &WorkspaceHash) -> Result<(), Error> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(FILE))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let directory = fs::symlink_metadata(root)?;
    let metadata = file.metadata()?;
    if !directory.is_dir()
        || directory.mode() & 0o077 != 0
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != directory.uid()
    {
        return Err(Error::Permissions);
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::Incompatible);
    }
    let record: Record = serde_json::from_slice(&bytes)?;
    if record.version != 1 || record.namespace != Namespace::from(workspace) {
        return Err(Error::Incompatible);
    }
    Err(Error::Pending)
}

impl Pending {
    pub fn begin(
        guard: &coordination::Guard,
        root: &Path,
        workspace: &WorkspaceHash,
        change: Change,
    ) -> Result<Self, Error> {
        if root.parent().and_then(Path::parent) != guard.root().parent()
            || root.file_name().and_then(|name| name.to_str()) != Some(workspace.as_str())
        {
            return Err(Error::Incompatible);
        }
        ensure_clear(root, workspace)?;
        let record = Record {
            version: 1,
            namespace: Namespace::from(workspace),
            change,
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join(FILE))?;
        file.write_all(&serde_json::to_vec(&record)?)?;
        file.sync_all()?;
        File::open(root)?.sync_all().map_err(Error::Committed)?;
        Ok(Self {
            root: root.to_path_buf(),
            change,
        })
    }

    /// Only a completed matching native request clears uncertainty; inspection and parent death do not.
    pub fn finish(self, observation: Observation) -> Result<(), Error> {
        if !matches!(
            (self.change, observation),
            (Change::Retire, Observation::Retired) | (Change::Resume, Observation::Active)
        ) {
            return Err(Error::Pending);
        }
        fs::remove_file(self.root.join(FILE))?;
        File::open(&self.root)?
            .sync_all()
            .map_err(Error::Committed)?;
        Ok(())
    }
}

#[cfg(test)]
mod test;
