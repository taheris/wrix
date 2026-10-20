//! Host-selected physical Celler ownership, separate from namespace cache demand.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    num::NonZeroU16,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;
use wrix_core::path::{ContainerName, WorkspaceHash};

use super::{CACHE_BACKEND_VERSION, Plan, RuntimeKind, coordination};

const FILE: &str = "celler-owner.json";
const LIMIT: u64 = 4096;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// selected Celler ownership I/O failed: {0}
    Io(#[from] io::Error),
    /// selected Celler ownership encoding failed: {0}
    Encoding(#[from] serde_json::Error),
    /// {0}
    Directory(#[from] wrix_core::private_directory::Error),
    /// selected Celler ownership must be an owner-only single-link regular file
    Permissions,
    /// selected Celler ownership has an invalid identity or unsupported version
    Invalid,
    /// selected Celler ownership changed or disappeared; explicit host reconciliation is required
    Conflict,
    /// selected Celler ownership was recorded, but durability could not be confirmed: {0}
    Committed(io::Error),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Selected {
    pub runtime: RuntimeKind,
    pub workspace: WorkspaceHash,
    pub container: ContainerName,
    pub port: NonZeroU16,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    schema_version: u8,
    backend_version: String,
    runtime: RuntimeKind,
    workspace_hash: String,
    container_name: String,
    cache_http_port: NonZeroU16,
}

impl Selected {
    pub fn owns(&self, plan: &Plan) -> bool {
        self.runtime == plan.namespace_runtime
            && self.workspace == *plan.workspace().hash()
            && self.container == plan.container_name()
    }

    pub fn from_plan(plan: &Plan, runtime: RuntimeKind) -> Result<Self, Error> {
        Ok(Self {
            runtime,
            workspace: plan.workspace().hash().clone(),
            container: plan.container_name(),
            port: plan
                .owned_cache_port()
                .and_then(NonZeroU16::new)
                .ok_or(Error::Invalid)?,
        })
    }

    /// Passive lookup never creates state, selects a backend or repairs metadata.
    pub fn read(root: &Path) -> Result<Option<Self>, Error> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join(FILE))
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let directory = fs::symlink_metadata(root)?;
        let metadata = file.metadata()?;
        if !directory.is_dir()
            || directory.mode() & 0o077 != 0
            || !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
            || metadata.uid() != directory.uid()
        {
            return Err(Error::Permissions);
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            return Err(Error::Invalid);
        }
        let wire: Wire = serde_json::from_slice(&bytes)?;
        if wire.schema_version != 1 || wire.backend_version != CACHE_BACKEND_VERSION {
            return Err(Error::Invalid);
        }
        Ok(Some(Self {
            runtime: wire.runtime,
            workspace: WorkspaceHash::parse(&wire.workspace_hash).map_err(|_| Error::Invalid)?,
            container: ContainerName::from_persisted(wire.container_name).ok_or(Error::Invalid)?,
            port: wire.cache_http_port,
        }))
    }

    /// Call only after native authenticated readiness, retaining host lifecycle exclusion.
    /// Existing selection is immutable here; this is not an update or release operation.
    pub fn record(&self, guard: &coordination::Guard) -> Result<(), Error> {
        let root = guard.root();
        if let Some(selected) = Self::read(root)? {
            return if selected == *self {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        let wire = Wire {
            schema_version: 1,
            backend_version: CACHE_BACKEND_VERSION.into(),
            runtime: self.runtime,
            workspace_hash: self.workspace.as_str().into(),
            container_name: self.container.as_str().into(),
            cache_http_port: self.port,
        };
        let mut temporary = tempfile::NamedTempFile::new_in(root)?;
        temporary.write_all(&serde_json::to_vec(&wire)?)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(root.join(FILE))
            .map_err(|error| error.error)?;
        File::open(root)?.sync_all().map_err(Error::Committed)?;
        Ok(())
    }
}

#[cfg(test)]
mod test;
