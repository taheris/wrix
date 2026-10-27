//! Explicit private-view invalidation with durable recording recovery.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use wrix_core::{cache_key::CachePublicKey, path::WorkspaceHash};

use super::{
    Error, Publisher, Result, acquire, private_directory, private_namespace, replace_key,
    retained_key,
};
use crate::backend::{Client, issuer::Authority};

const JOURNAL: &str = "celler-rotation.json";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    workspace: String,
    endpoint: String,
    issuer: PathBuf,
    retention_seconds: u64,
    prior_key: Option<String>,
}

pub fn ensure_clear(root: &Path) -> Result<()> {
    match fs::symlink_metadata(root.join(JOURNAL)) {
        Ok(_) => Err(Error::PendingRotation),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Explicitly invalidates one verified private view, or finishes its interrupted local recording.
/// Callers retain workspace and host lifecycle exclusion and verify the backend and issuer identity.
pub async fn rotate_private(
    state_root: &Path,
    workspace: &WorkspaceHash,
    endpoint: &str,
    issuer_root: &Path,
    retention: Duration,
    lock_timeout: Duration,
) -> Result<CachePublicKey> {
    super::super::super::retention_seconds(retention)?;
    let namespace = private_namespace(workspace)?;
    let token = Authority::open(issuer_root)?.publication_token(&namespace)?;
    let client = Client::new(endpoint, namespace.clone(), &token)?;
    wrix_core::private_directory::prepare_with_parents(state_root)?;
    let lock = acquire(state_root, lock_timeout).await?;
    let intent = read_intent(state_root)?;
    if super::super::retirement::is_retired(state_root, workspace)? && intent.is_none() {
        return Err(Error::Retired);
    }
    validate_binding(state_root, workspace, endpoint, issuer_root, &namespace)?;
    retained_key(state_root)?;
    let recorded = intent
        .map(|intent| {
            if intent.workspace != workspace.as_str()
                || intent.endpoint != endpoint
                || intent.issuer != issuer_root
                || intent.retention_seconds != retention.as_secs()
            {
                return Err(Error::Binding);
            }
            intent
                .prior_key
                .as_deref()
                .map(CachePublicKey::parse)
                .transpose()
                .map_err(|_| Error::Binding)
        })
        .transpose()?;
    let session = super::super::workflow::Session::open(&client).await?;
    let client = session.client();
    let prior = async {
        if let Some(prior) = recorded {
            return Ok(prior);
        }
        let prior = current_key(client).await?;
        let intent = Intent {
            workspace: workspace.as_str().to_owned(),
            endpoint: endpoint.to_owned(),
            issuer: issuer_root.to_owned(),
            retention_seconds: retention.as_secs(),
            prior_key: prior.as_ref().map(|key| key.as_str().to_owned()),
        };
        write_intent(state_root, &intent)?;
        Ok::<_, Error>(prior)
    }
    .await;
    let prior = match prior {
        Ok(prior) => prior,
        Err(error) => {
            session.finish().await.map_err(Error::WorkflowCompletion)?;
            return Err(error);
        }
    };
    let operation = async {
        let current = current_key(client).await?;
        let key = if prior.is_some() && current == prior {
            client.rotate(retention).await?
        } else {
            client.register(retention).await?
        };
        replace_key(state_root, &key)?;
        Publisher::install_issuer(
            state_root,
            workspace,
            endpoint,
            namespace,
            issuer_root,
            retention,
        )?;
        Ok(key)
    };
    let result = operation.await;
    session.finish().await.map_err(Error::WorkflowCompletion)?;
    let key = result.map_err(|source| Error::RotationIncomplete {
        source: Box::new(source),
    })?;
    fs::remove_file(state_root.join(JOURNAL)).map_err(|source| Error::RotationIncomplete {
        source: Box::new(source.into()),
    })?;
    File::open(state_root)
        .and_then(|directory| directory.sync_all())
        .map_err(Error::RotationRecorded)?;
    fs2::FileExt::unlock(&lock).map_err(Error::UnlockCommitted)?;
    Ok(key)
}

async fn current_key(client: &Client) -> Result<Option<CachePublicKey>> {
    client
        .configuration()
        .await?
        .map(|config| crate::backend::parse_public_key(&config))
        .transpose()
        .map_err(Error::from)
}

fn validate_binding(
    root: &Path,
    workspace: &WorkspaceHash,
    endpoint: &str,
    issuer: &Path,
    namespace: &attic::cache::CacheName,
) -> Result<()> {
    match super::super::open_binding(root) {
        Ok(_) => {
            let publisher = Publisher::load_authority(root, workspace)?;
            match publisher.authority {
                super::super::Credential::Issuer {
                    root,
                    endpoint: bound_endpoint,
                    namespace: bound_namespace,
                } if root == issuer
                    && bound_endpoint == endpoint
                    && &bound_namespace == namespace =>
                {
                    Ok(())
                }
                _ => Err(Error::Binding),
            }
        }
        Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn read_intent(root: &Path) -> Result<Option<Intent>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(JOURNAL))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    super::super::check_file(&file.metadata()?, &private_directory(root)?)?;
    let mut bytes = Vec::new();
    file.take(super::super::MAX_BINDING_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > super::super::MAX_BINDING_BYTES {
        return Err(Error::Binding);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| Error::Binding)
}

fn write_intent(root: &Path, intent: &Intent) -> Result<()> {
    let bytes = serde_json::to_vec(intent).map_err(|_| Error::Binding)?;
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(root.join(JOURNAL))
        .map_err(|error| error.error)?;
    File::open(root)?.sync_all()?;
    let parent = root.parent().ok_or(Error::Permissions)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
