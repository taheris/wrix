//! Cache issuance from the current host registry, never stale bearer claims.

use std::{
    path::{Component, Path},
    time::{Duration, UNIX_EPOCH},
};

use displaydoc::Display;
use thiserror::Error as ThisError;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::timeout,
};
use wrix_cache::backend::{
    issuer::Authority,
    pull::{Audience, Credential},
};
use wrix_core::path::WorkspaceHash;

use super::{Access, Client, Controller, IO_BUDGET, Namespace, Operation, Reply};

const MAX_BEARER: usize = 16 * 1024;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// host cache authorization failed: {0}
    Control(#[from] super::Error),
    /// host cache credential signing failed: {0}
    Issuer(#[from] wrix_cache::backend::issuer::Error),
}

impl Controller {
    /// Configure host-selected private authority; requests can neither choose nor provision it.
    pub fn with_cache_issuer(mut self, root: &Path) -> Result<Self, super::Error> {
        if !root.is_absolute()
            || root
                .components()
                .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
        {
            return Err(super::Error::PrivateRoot);
        }
        self.cache_issuer = Some(root.to_owned());
        Ok(self)
    }

    pub(super) async fn serve_cache_credential(
        &self,
        stream: &mut UnixStream,
        namespace: &Namespace,
    ) -> Result<(), super::Error> {
        let issuer = self
            .cache_issuer
            .as_ref()
            .ok_or(super::Error::CacheUnavailable)?;
        let issuer = Authority::open(issuer).map_err(|_| super::Error::CacheUnavailable)?;
        let workspace =
            WorkspaceHash::parse(&namespace.0).map_err(|_| super::Error::UnknownNamespace)?;
        let credential = self
            .cache_credential(&issuer, &workspace)
            .map_err(|_| super::Error::CacheUnavailable)?;
        let bearer = credential.expose_bearer().as_bytes();
        if bearer.len() > MAX_BEARER {
            return Err(super::Error::FrameLimit);
        }
        let expires_at = credential
            .expires_at()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| super::Error::CacheUnavailable)?
            .as_secs();
        super::send(stream, &Reply::CacheIssued { expires_at }).await?;
        stream
            .write_u32(u32::try_from(bearer.len()).map_err(|_| super::Error::FrameLimit)?)
            .await?;
        stream.write_all(bearer).await?;
        Ok(())
    }

    /// Rechecks both audiences and excludes retirement until signing finishes.
    pub fn cache_credential(
        &self,
        issuer: &Authority,
        workspace: &WorkspaceHash,
    ) -> Result<Credential, Error> {
        let registry = self
            .namespaces
            .lock()
            .map_err(|_| super::Error::LockPoisoned)?;
        let policy = registry
            .get(&Namespace::from(workspace))
            .ok_or(super::Error::UnknownNamespace)?;
        let audience = match (policy.common_cache_read, policy.private_cache_read) {
            (Access::Allowed, Access::Allowed) => Audience::CommonAndPrivate,
            (Access::Allowed, Access::Denied) => Audience::Common,
            (Access::Denied, Access::Allowed) => Audience::Private,
            (Access::Denied, Access::Denied) => return Err(super::Error::PermissionDenied.into()),
        };
        let credential = issuer.restricted_pull_credential(workspace, audience);
        drop(registry);
        Ok(credential?)
    }
}

impl Client {
    /// Receives an opaque grant over private authenticated IPC, never diagnostic JSON.
    pub async fn cache_credential(
        &self,
        namespace: Namespace,
    ) -> Result<wrix_sandbox::cache_credentials::Grant, super::Error> {
        timeout(IO_BUDGET, async {
            let (mut stream, reply) = self.exchange(Operation::IssueCache { namespace }).await?;
            let Reply::CacheIssued { expires_at } = reply else {
                return Err(super::Error::CacheUnavailable);
            };
            let length =
                usize::try_from(stream.read_u32().await?).map_err(|_| super::Error::FrameLimit)?;
            if length == 0 || length > MAX_BEARER {
                return Err(super::Error::FrameLimit);
            }
            let mut bearer = vec![0; length];
            stream.read_exact(&mut bearer).await?;
            let bearer = String::from_utf8(bearer).map_err(|_| super::Error::CacheUnavailable)?;
            let expires_at = UNIX_EPOCH
                .checked_add(Duration::from_secs(expires_at))
                .ok_or(super::Error::CacheUnavailable)?;
            wrix_sandbox::cache_credentials::Grant::new(bearer, expires_at)
                .map_err(|_| super::Error::CacheUnavailable)
        })
        .await
        .map_err(|_| super::Error::Timeout)?
    }
}

#[cfg(test)]
mod test;
