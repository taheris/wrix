//! Closed startup inputs; public bootstrap data is not host admission authority.

pub(crate) mod ownership;

use super::{Error, image::StorePath, lease::Key};
use serde::{Deserialize, Serialize};

pub const CAPACITY: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Storage {
    Allocate,
    Preallocated,
    Resume,
    Upgrade,
}

impl Storage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allocate => "allocate",
            Self::Preallocated => "preallocated",
            Self::Resume => "resume",
            Self::Upgrade => "upgrade",
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u16,
    admin_key: Key,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage: Option<Storage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upgrade_from: Option<StorePath>,
}

#[derive(Deserialize, Serialize)]
#[serde(try_from = "Wire", into = "Wire")]
#[derive(Clone)]
pub struct Configuration {
    pub(super) admin_key: Key,
    storage: Storage,
    upgrade_from: Option<StorePath>,
}

impl Configuration {
    pub const fn preallocated(admin_key: Key) -> Self {
        Self {
            admin_key,
            storage: Storage::Preallocated,
            upgrade_from: None,
        }
    }

    pub const fn resume(admin_key: Key) -> Self {
        Self {
            admin_key,
            storage: Storage::Resume,
            upgrade_from: None,
        }
    }

    pub const fn upgrade(admin_key: Key, previous: StorePath) -> Self {
        Self {
            admin_key,
            storage: Storage::Upgrade,
            upgrade_from: Some(previous),
        }
    }

    pub const fn upgrade_from(&self) -> Option<&StorePath> {
        self.upgrade_from.as_ref()
    }

    pub const fn admin_key(&self) -> &Key {
        &self.admin_key
    }

    pub const fn storage(&self) -> Storage {
        self.storage
    }
}

impl TryFrom<Wire> for Configuration {
    type Error = Error;

    fn try_from(wire: Wire) -> Result<Self, Error> {
        let storage = match (wire.version, wire.storage, &wire.upgrade_from) {
            (1, None, None) => Storage::Allocate,
            (2, Some(Storage::Preallocated), None) => Storage::Preallocated,
            (3, Some(Storage::Resume), None) => Storage::Resume,
            (4, Some(Storage::Upgrade), Some(_)) => Storage::Upgrade,
            _ => return Err(Error::Invalid),
        };
        Ok(Self {
            admin_key: wire.admin_key,
            storage,
            upgrade_from: wire.upgrade_from,
        })
    }
}

impl From<Configuration> for Wire {
    fn from(configuration: Configuration) -> Self {
        let (version, storage) = match configuration.storage {
            Storage::Allocate => (1, None),
            Storage::Preallocated => (2, Some(Storage::Preallocated)),
            Storage::Resume => (3, Some(Storage::Resume)),
            Storage::Upgrade => (4, Some(Storage::Upgrade)),
        };
        Self {
            version,
            admin_key: configuration.admin_key,
            storage,
            upgrade_from: configuration.upgrade_from,
        }
    }
}

#[cfg(test)]
mod test;
