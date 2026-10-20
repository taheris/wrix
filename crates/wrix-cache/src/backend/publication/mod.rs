//! Runtime host authority for one private publication view.

pub mod registration;
pub mod retirement;

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

#[cfg(test)]
mod test;

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
    /// invalid host-private publication directory: {0}
    Directory(#[from] wrix_core::private_directory::Error),
    /// namespace lock and public key must be single-link regular files owned by the control directory owner, without group/other write access
    UnsafeRegistrationFile,
    /// native registration reported a different key from retained valid namespace metadata; explicit key reconciliation is required
    RetainedKeyConflict,
    /// timed out waiting for the namespace publication lock; no backend request was dispatched
    LockTimeout,
    /// Celler registration succeeded, but local key or publication authority recording failed; backend changes remain active: {source}
    RegistrationCommitted { source: Box<Error> },
    /// {0}
    Issuer(#[from] super::issuer::Error),
    /// Celler publication binding belongs to a different workspace
    Workspace,
    /// namespace publication is retired; authenticated service startup is required
    Retired,
    /// namespace publication was retired, but directory durability could not be confirmed: {0}
    RetirementCommitted(io::Error),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    issuer_root: Option<PathBuf>,
    retention_seconds: u64,
}

/// Resolved host authority; it deliberately has no serialization or diagnostic representation.
pub struct Publisher {
    authority: Credential,
    retention: Duration,
}

enum Credential {
    Token(Client),
    Issuer {
        root: PathBuf,
        endpoint: String,
        namespace: CacheName,
    },
}

impl Credential {
    fn client(&self) -> Result<std::borrow::Cow<'_, Client>> {
        match self {
            Self::Token(client) => Ok(std::borrow::Cow::Borrowed(client)),
            Self::Issuer {
                root,
                endpoint,
                namespace,
            } => {
                let token = super::issuer::Authority::open(root)?.publication_token(namespace)?;
                Ok(std::borrow::Cow::Owned(Client::new(
                    endpoint,
                    namespace.clone(),
                    &token,
                )?))
            }
        }
    }
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
            token: Some(token.to_owned()),
            issuer_root: None,
            retention_seconds: retention.as_secs(),
        };
        Self::install_binding(state_root, &binding)
    }

    /// Installs host-only issuance coordinates, not an expiring credential snapshot.
    /// Each operation reopens the issuer and issues exact-view authority at the current clock time.
    pub fn install_issuer(
        state_root: &Path,
        workspace: &WorkspaceHash,
        endpoint: &str,
        namespace: CacheName,
        issuer_root: &Path,
        retention: Duration,
    ) -> Result<()> {
        super::retention_seconds(retention)?;
        let token = super::issuer::Authority::open(issuer_root)?.publication_token(&namespace)?;
        Client::new(endpoint, namespace.clone(), &token)?;
        Self::install_binding(
            state_root,
            &Binding {
                schema_version: 2,
                workspace_hash: workspace.as_str().to_owned(),
                endpoint: endpoint.to_owned(),
                namespace,
                token: None,
                issuer_root: Some(issuer_root.to_owned()),
                retention_seconds: retention.as_secs(),
            },
        )
    }

    fn install_binding(state_root: &Path, binding: &Binding) -> Result<()> {
        let bytes = serde_json::to_vec(binding).map_err(|_| Error::Binding)?;
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
        if retirement::is_retired(state_root, workspace)? {
            return Err(Error::Retired);
        }
        Self::load_authority(state_root, workspace)
    }

    /// Retains policy across retirement without admitting publication.
    pub fn retained_policy(state_root: &Path, workspace: &WorkspaceHash) -> Result<Duration> {
        retirement::is_retired(state_root, workspace)?;
        Ok(Self::load_authority(state_root, workspace)?.retention())
    }

    fn load_authority(state_root: &Path, workspace: &WorkspaceHash) -> Result<Self> {
        let directory = private_directory(state_root)?;
        let file = open_binding(state_root)?;
        check_file(&file.metadata()?, &directory)?;
        let mut bytes = Vec::new();
        file.take(MAX_BINDING_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BINDING_BYTES {
            return Err(Error::Binding);
        }
        let binding: Binding = serde_json::from_slice(&bytes).map_err(|_| Error::Binding)?;
        let bound_workspace =
            WorkspaceHash::parse(&binding.workspace_hash).map_err(|_| Error::Binding)?;
        if bound_workspace != *workspace {
            return Err(Error::Workspace);
        }
        let retention = Duration::from_secs(binding.retention_seconds);
        super::retention_seconds(retention)?;
        let authority = match (binding.schema_version, binding.token, binding.issuer_root) {
            (1, Some(token), None) => {
                Credential::Token(Client::new(&binding.endpoint, binding.namespace, &token)?)
            }
            (2, None, Some(root)) => {
                let token =
                    super::issuer::Authority::open(&root)?.publication_token(&binding.namespace)?;
                Client::new(&binding.endpoint, binding.namespace.clone(), &token)?;
                Credential::Issuer {
                    root,
                    endpoint: binding.endpoint,
                    namespace: binding.namespace,
                }
            }
            _ => return Err(Error::Binding),
        };
        Ok(Self {
            authority,
            retention,
        })
    }

    /// The finite host-selected policy retained by this publication binding.
    pub const fn retention(&self) -> Duration {
        self.retention
    }

    /// Missing paths remain missing; native store and backend failures propagate unchanged.
    pub async fn publish_selected(&self, paths: &[PathBuf]) -> Result<usize> {
        Ok(self.authority.client()?.publish_selected(paths).await?)
    }

    /// Callers serialize registration with the selected namespace's publication lock.
    pub async fn register(&self) -> Result<CachePublicKey> {
        Ok(self.authority.client()?.register(self.retention).await?)
    }

    /// Callers serialize rotation with the selected namespace's publication lock.
    pub async fn rotate(&self) -> Result<CachePublicKey> {
        Ok(self.authority.client()?.rotate(self.retention).await?)
    }
}
