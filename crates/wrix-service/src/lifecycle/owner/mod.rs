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

pub(super) mod host;

use super::{CACHE_BACKEND_VERSION, Plan, RuntimeKind, binding, coordination};
use wrix_sandbox::image::Digest;

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
    pub workspace: Option<WorkspaceHash>,
    pub container: ContainerName,
    pub port: NonZeroU16,
    pub recipe: Option<Recipe>,
}

/// Package provenance and immutable native image identity of a newly created owner.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Recipe {
    pub binding: binding::Candidate,
    pub image_id: Digest,
    pub config_digest: Digest,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    schema_version: u8,
    backend_version: String,
    runtime: RuntimeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace_hash: Option<String>,
    container_name: String,
    cache_http_port: NonZeroU16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recipe: Option<Recipe>,
}

impl Selected {
    pub fn owns(&self, plan: &Plan) -> bool {
        self.runtime == plan.namespace_runtime
            && self.workspace.as_ref() == Some(plan.workspace().hash())
            && self.container == plan.container_name()
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
        if !matches!(
            (
                wire.schema_version,
                wire.recipe.is_some(),
                wire.workspace_hash.is_some()
            ),
            (1, false, true) | (2, true, true) | (3, _, false)
        ) || wire.backend_version != CACHE_BACKEND_VERSION
        {
            return Err(Error::Invalid);
        }
        Ok(Some(Self {
            runtime: wire.runtime,
            workspace: wire
                .workspace_hash
                .as_deref()
                .map(WorkspaceHash::parse)
                .transpose()
                .map_err(|_| Error::Invalid)?,
            container: ContainerName::from_persisted(wire.container_name).ok_or(Error::Invalid)?,
            port: wire.cache_http_port,
            recipe: wire.recipe,
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
        self.persist(root)
    }

    /// Replace provenance only for the same physical owner, after coordinated recovery readiness.
    pub fn recovered(&self, previous: &Self, guard: &coordination::Guard) -> Result<(), Error> {
        if Self::read(guard.root())?.as_ref() != Some(previous)
            || self.runtime != previous.runtime
            || self.workspace != previous.workspace
            || self.container != previous.container
            || self.port != previous.port
        {
            return Err(Error::Conflict);
        }
        self.persist(guard.root())
    }

    fn persist(&self, root: &Path) -> Result<(), Error> {
        let wire = Wire {
            schema_version: if self.workspace.is_none() {
                3
            } else if self.recipe.is_some() {
                2
            } else {
                1
            },
            backend_version: CACHE_BACKEND_VERSION.into(),
            runtime: self.runtime,
            workspace_hash: self
                .workspace
                .as_ref()
                .map(|workspace| workspace.as_str().into()),
            container_name: self.container.as_str().into(),
            cache_http_port: self.port,
            recipe: self.recipe.clone(),
        };
        let bytes = serde_json::to_vec(&wire)?;
        if bytes.len() as u64 > LIMIT {
            return Err(Error::Invalid);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(root)?;
        temporary.write_all(&bytes)?;
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
