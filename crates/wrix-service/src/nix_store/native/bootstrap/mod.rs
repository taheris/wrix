//! Closed startup inputs; public bootstrap data is not host admission authority.

use super::{Error, lease::Key};
use serde::{Deserialize, Serialize};

pub const CAPACITY: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Storage {
    Allocate,
    Preallocated,
    Resume,
}

impl Storage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allocate => "allocate",
            Self::Preallocated => "preallocated",
            Self::Resume => "resume",
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
}

#[derive(Deserialize, Serialize)]
#[serde(try_from = "Wire", into = "Wire")]
#[derive(Clone)]
pub struct Configuration {
    pub(super) admin_key: Key,
    storage: Storage,
}

impl Configuration {
    pub const fn preallocated(admin_key: Key) -> Self {
        Self {
            admin_key,
            storage: Storage::Preallocated,
        }
    }

    pub const fn storage(&self) -> Storage {
        self.storage
    }
}

impl TryFrom<Wire> for Configuration {
    type Error = Error;

    fn try_from(wire: Wire) -> Result<Self, Error> {
        let storage = match (wire.version, wire.storage) {
            (1, None) => Storage::Allocate,
            (2, Some(Storage::Preallocated)) => Storage::Preallocated,
            (3, Some(Storage::Resume)) => Storage::Resume,
            _ => return Err(Error::Invalid),
        };
        Ok(Self {
            admin_key: wire.admin_key,
            storage,
        })
    }
}

impl From<Configuration> for Wire {
    fn from(configuration: Configuration) -> Self {
        let (version, storage) = match configuration.storage {
            Storage::Allocate => (1, None),
            Storage::Preallocated => (2, Some(Storage::Preallocated)),
            Storage::Resume => (3, Some(Storage::Resume)),
        };
        Self {
            version,
            admin_key: configuration.admin_key,
            storage,
        }
    }
}

#[cfg(test)]
mod test;
