//! Runtime host authority for one private publication view.

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use attic::cache::CacheName;
use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;
use wrix_core::{cache_key::CachePublicKey, path::WorkspaceHash};

use super::Client;

const MAX_BINDING_BYTES: u64 = 16 * 1024;

fn private_directory(root: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.mode() & 0o077 != 0 {
        return Err(Error::Permissions);
    }
    Ok(metadata)
}

fn open_binding(root: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join("celler-publisher.json"))?)
}

fn check_file(metadata: &fs::Metadata, directory: &fs::Metadata) -> Result<()> {
    if !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != directory.uid()
        || metadata.nlink() != 1
    {
        return Err(Error::Permissions);
    }
    Ok(())
}

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
    /// Celler publication authority was replaced, but directory durability could not be confirmed: {0}
    InstallationCommitted(io::Error),
}

#[derive(Deserialize, Serialize)]
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
    /// Atomically installs host-selected authority; callers hold the namespace lock after privilege drop.
    /// The absolute parent directory must exist; validation precedes mutation.
    /// This private file is never an immutable hook snapshot.
    pub fn install(
        state_root: &Path,
        workspace: &WorkspaceHash,
        endpoint: &str,
        namespace: CacheName,
        token: &str,
        retention: Duration,
    ) -> Result<()> {
        super::retention_seconds(retention)?;
        let _client = Client::new(endpoint, namespace.clone(), token)?;
        let binding = Binding {
            schema_version: 1,
            workspace_hash: workspace.as_str().to_owned(),
            endpoint: endpoint.to_owned(),
            namespace,
            token: token.to_owned(),
            retention_seconds: retention.as_secs(),
        };
        let bytes = serde_json::to_vec(&binding).map_err(|_| Error::Binding)?;
        if bytes.len() as u64 > MAX_BINDING_BYTES {
            return Err(Error::Binding);
        }
        let parent = state_root
            .parent()
            .filter(|parent| parent.is_absolute())
            .ok_or(Error::Permissions)?;
        match DirBuilder::new().mode(0o700).create(state_root) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        let directory = private_directory(state_root)?;
        match open_binding(state_root) {
            Ok(file) => check_file(&file.metadata()?, &directory)?,
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        let mut file = tempfile::NamedTempFile::new_in(state_root)?;
        check_file(&file.as_file().metadata()?, &directory)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(state_root.join("celler-publisher.json"))
            .map_err(|error| Error::Io(error.error))?;
        File::open(state_root)
            .and_then(|directory| directory.sync_all())
            .and_then(|()| File::open(parent)?.sync_all())
            .map_err(Error::InstallationCommitted)?;
        Ok(())
    }

    /// Reads only host control state; no environment, repository configuration or service mounts.
    pub fn load(state_root: &Path, workspace: &WorkspaceHash) -> Result<Self> {
        let directory = private_directory(state_root)?;
        let file = open_binding(state_root)?;
        check_file(&file.metadata()?, &directory)?;
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
