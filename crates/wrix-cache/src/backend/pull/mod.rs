//! Exact native read grants; host authorization must precede issuance.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use attic::cache::CacheName;
use displaydoc::Display;
use thiserror::Error as ThisError;
use wrix_core::path::WorkspaceHash;

use super::publication::registration;

pub(super) const LIFETIME_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// canonical cache identity cannot be represented as an exact native view
    Identity,
    /// host clock cannot represent the sandbox pull credential lifetime
    Clock,
}

/// Nonempty subsets of the two canonical read audiences, never caller-chosen cache names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Audience {
    Common,
    Private,
    CommonAndPrivate,
}

impl Audience {
    pub const fn common(self) -> bool {
        matches!(self, Self::Common | Self::CommonAndPrivate)
    }

    pub const fn private(self) -> bool {
        matches!(self, Self::Private | Self::CommonAndPrivate)
    }
}

/// Canonical view identities and the exact admitted subset.
pub struct ReadSet {
    common: CacheName,
    private: CacheName,
    audience: Audience,
}

impl ReadSet {
    pub fn common_name() -> Result<CacheName, Error> {
        CacheName::new(String::from("wrix-common-v1")).map_err(|_| Error::Identity)
    }

    pub fn for_workspace(workspace: &WorkspaceHash) -> Result<Self, Error> {
        Self::admit(workspace, Audience::CommonAndPrivate)
    }

    pub fn admit(workspace: &WorkspaceHash, audience: Audience) -> Result<Self, Error> {
        Ok(Self {
            common: Self::common_name()?,
            private: registration::private_namespace(workspace).map_err(|_| Error::Identity)?,
            audience,
        })
    }

    pub const fn audience(&self) -> Audience {
        self.audience
    }

    pub fn granted(&self) -> impl Iterator<Item = &CacheName> {
        [
            self.audience.common().then_some(&self.common),
            self.audience.private().then_some(&self.private),
        ]
        .into_iter()
        .flatten()
    }

    pub const fn common(&self) -> &CacheName {
        &self.common
    }

    pub const fn private(&self) -> &CacheName {
        &self.private
    }
}

/// Runtime-only bearer authority; never diagnostic or serializable configuration.
pub struct Credential {
    token: String,
    expires_at: SystemTime,
    views: ReadSet,
}

impl Credential {
    pub(super) fn new(token: String, expires: u64, views: ReadSet) -> Result<Self, Error> {
        let expires_at = UNIX_EPOCH
            .checked_add(Duration::from_secs(expires))
            .ok_or(Error::Clock)?;
        Ok(Self {
            token,
            expires_at,
            views,
        })
    }

    pub fn expose_bearer(&self) -> &str {
        &self.token
    }

    pub const fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    pub const fn views(&self) -> &ReadSet {
        &self.views
    }
}
