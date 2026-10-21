//! Cache issuance from the current host registry, never stale bearer claims.

use displaydoc::Display;
use thiserror::Error as ThisError;
use wrix_cache::backend::{
    issuer::Authority,
    pull::{Audience, Credential},
};
use wrix_core::path::WorkspaceHash;

use super::{Access, Controller, Namespace};

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// host cache authorization failed: {0}
    Control(#[from] super::Error),
    /// host cache credential signing failed: {0}
    Issuer(#[from] wrix_cache::backend::issuer::Error),
}

impl Controller {
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

#[cfg(test)]
mod test;
