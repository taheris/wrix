//! Runtime host authority for one private publication view.

use std::{
    fs::{self, OpenOptions},
    io::{self, Read},
    os::unix::{fs::MetadataExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use attic::cache::CacheName;
use displaydoc::Display;
use serde::Deserialize;
use thiserror::Error as ThisError;
use wrix_core::{cache_key::CachePublicKey, path::WorkspaceHash};

use super::Client;

const MAX_BINDING_BYTES: u64 = 16 * 1024;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// cannot read host-private Celler publication authority: {0}
    Io(#[from] io::Error),
    /// Celler publication authority must be an owner-only regular file in a private directory
    Permissions,
    /// invalid host-private Celler publication binding
    Binding,
    /// Celler publication binding belongs to a different workspace
    Workspace,
    /// {0}
    Backend(#[from] super::Error),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    schema_version: u8,
    workspace_hash: String,
    endpoint: String,
    namespace: CacheName,
    token: String,
    retention_seconds: u64,
}

/// Resolved host authority; it deliberately has no serialization or diagnostic representation.
pub struct Publisher {
    client: Client,
    retention: Duration,
}

impl Publisher {
    /// Reads only host control state; no environment, repository configuration or service mounts.
    pub fn load(state_root: &Path, workspace: &WorkspaceHash) -> Result<Self> {
        let directory = fs::symlink_metadata(state_root)?;
        if !directory.is_dir() || directory.mode() & 0o077 != 0 {
            return Err(Error::Permissions);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(state_root.join("celler-publisher.json"))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != directory.uid()
            || metadata.nlink() != 1
        {
            return Err(Error::Permissions);
        }
        let mut bytes = Vec::new();
        file.take(MAX_BINDING_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BINDING_BYTES {
            return Err(Error::Binding);
        }
        let binding: Binding = serde_json::from_slice(&bytes).map_err(|_| Error::Binding)?;
        if binding.schema_version != 1 {
            return Err(Error::Binding);
        }
        let bound_workspace =
            WorkspaceHash::parse(&binding.workspace_hash).map_err(|_| Error::Binding)?;
        if bound_workspace != *workspace {
            return Err(Error::Workspace);
        }
        let retention = Duration::from_secs(binding.retention_seconds);
        super::retention_seconds(retention)?;
        Ok(Self {
            client: Client::new(&binding.endpoint, binding.namespace, &binding.token)?,
            retention,
        })
    }

    /// Missing paths remain missing; native store and backend failures propagate unchanged.
    pub async fn publish_selected(&self, paths: &[PathBuf]) -> Result<usize> {
        Ok(self.client.publish_selected(paths).await?)
    }

    /// Callers serialize registration with the selected namespace's publication lock.
    pub async fn register(&self) -> Result<CachePublicKey> {
        Ok(self.client.register(self.retention).await?)
    }

    /// Callers serialize rotation with the selected namespace's publication lock.
    pub async fn rotate(&self) -> Result<CachePublicKey> {
        Ok(self.client.rotate(self.retention).await?)
    }
}
