//! Authenticated private-view registration and durable host publication admission.

use std::{
    fs::{File, Metadata, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::Duration,
};

use attic::cache::CacheName;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use tokio::time::Instant;
use wrix_core::{cache_key::CachePublicKey, path::WorkspaceHash};

use super::{Error, Publisher, Result, private_directory};
use crate::backend::{Client, issuer::Authority};

/// Confirms and records a private view's key, native policy and issuer-backed publication authority.
///
/// Callers provide host-controlled paths and lifecycle coordination; success is not substitution proof.
pub async fn provision_private(
    state_root: &Path,
    workspace: &WorkspaceHash,
    endpoint: &str,
    issuer_root: &Path,
    retention: Duration,
    lock_timeout: Duration,
) -> Result<CachePublicKey> {
    super::super::retention_seconds(retention)?;
    let namespace = private_namespace(workspace)?;
    let token = Authority::open(issuer_root)?.publication_token(&namespace)?;
    let client = Client::new(endpoint, namespace.clone(), &token)?;
    wrix_core::private_directory::prepare_with_parents(state_root)?;
    let _lock = acquire(state_root, lock_timeout).await?;
    let key = client.register(retention).await?;
    let record = || {
        record_key(state_root, &key)?;
        Publisher::install_issuer(
            state_root,
            workspace,
            endpoint,
            namespace,
            issuer_root,
            retention,
        )?;
        super::retirement::activate(state_root, workspace)
    };
    record().map_err(|source| Error::RegistrationCommitted {
        source: Box::new(source),
    })?;
    Ok(key)
}

/// Confirms an authenticated common read view without granting publication admission.
pub async fn provision_common(
    state_root: &Path,
    endpoint: &str,
    issuer_root: &Path,
    retention: Duration,
    lock_timeout: Duration,
) -> Result<CachePublicKey> {
    super::super::retention_seconds(retention)?;
    let namespace = crate::backend::pull::ReadSet::common_name().map_err(|_| Error::Binding)?;
    let token = Authority::open(issuer_root)?.publication_token(&namespace)?;
    let client = Client::new(endpoint, namespace, &token)?;
    wrix_core::private_directory::prepare_with_parents(state_root)?;
    let _lock = acquire(state_root, lock_timeout).await?;
    let retention = match client.configuration().await? {
        None => retention,
        Some(config) => match config.retention_period {
            Some(attic::api::v1::cache_config::RetentionPeriodConfig::Period(seconds))
                if seconds > 0 =>
            {
                Duration::from_secs(u64::from(seconds))
            }
            _ => return Err(crate::backend::Error::Retention.into()),
        },
    };
    let key = client.register(retention).await?;
    record_key(state_root, &key).map_err(|source| Error::RegistrationCommitted {
        source: Box::new(source),
    })?;
    Ok(key)
}

/// Encodes the entire canonical identity without truncation within Celler's native name limit.
pub fn private_namespace(workspace: &WorkspaceHash) -> Result<CacheName> {
    let digest = workspace
        .as_str()
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digits = std::str::from_utf8(pair).map_err(|_| Error::Binding)?;
            u8::from_str_radix(digits, 16).map_err(|_| Error::Binding)
        })
        .collect::<Result<Vec<_>>>()?;
    CacheName::new(format!("wrix-{}", URL_SAFE_NO_PAD.encode(digest))).map_err(|_| Error::Binding)
}

pub(super) async fn acquire(state_root: &Path, timeout: Duration) -> Result<File> {
    let directory = private_directory(state_root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(state_root.join("cache.lock"))?;
    check_regular(&lock.metadata()?, &directory)?;
    let started = Instant::now();
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(lock),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if started.elapsed() >= timeout {
                    return Err(Error::LockTimeout);
                }
                tokio::time::sleep(
                    Duration::from_millis(50).min(timeout.saturating_sub(started.elapsed())),
                )
                .await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn record_key(state_root: &Path, key: &CachePublicKey) -> Result<()> {
    let keys = state_root.join("keys");
    wrix_core::private_directory::prepare_with_parents(&keys)?;
    let directory = private_directory(&keys)?;
    let destination = keys.join("cache.pub");
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&destination)
    {
        Ok(file) => {
            check_regular(&file.metadata()?, &directory)?;
            let mut bytes = Vec::new();
            file.take(super::MAX_BINDING_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > super::MAX_BINDING_BYTES {
                return Err(Error::Binding);
            }
            // Only malformed public metadata is repairable from authenticated native state.
            if let Ok(text) = std::str::from_utf8(&bytes)
                && let Ok(retained) = CachePublicKey::parse(text.trim())
            {
                return if &retained == key {
                    Ok(())
                } else {
                    Err(Error::RetainedKeyConflict)
                };
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&keys)?;
    writeln!(temporary, "{}", key.as_str())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&destination)
        .map_err(|error| error.error)?;
    File::open(&keys)?.sync_all()?;
    File::open(state_root)?.sync_all()?;
    Ok(())
}

fn check_regular(metadata: &Metadata, directory: &Metadata) -> Result<()> {
    if !metadata.is_file()
        || metadata.uid() != directory.uid()
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
    {
        return Err(Error::UnsafeRegistrationFile);
    }
    Ok(())
}

#[cfg(test)]
mod test;
