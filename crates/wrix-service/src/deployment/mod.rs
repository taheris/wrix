//! Host-selected deployment identity and paths, independent of checkout and generation lifetimes.

pub mod selection;

use std::{
    fmt,
    num::NonZeroU64,
    path::{Component, Path, PathBuf},
};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;

use crate::control::Namespace;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// deployment identity must contain exactly 32 lowercase hexadecimal characters
    Identity,
    /// host state and cache bases must be absolute normalized paths
    Path,
}

/// Host-created random identity; neither repository hashes nor container reports select it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);

impl TryFrom<String> for Id {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(Error::Identity);
        }
        Ok(Self(value))
    }
}

impl From<Id> for String {
    fn from(value: Id) -> Self {
        value.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A deployment-local generation number, not a workspace identity or readiness assertion.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Generation(NonZeroU64);

impl Generation {
    pub const fn new(number: NonZeroU64) -> Self {
        Self(number)
    }

    pub const fn number(self) -> NonZeroU64 {
        self.0
    }
}

/// Independently owned native backend data; a path grants no right to open it concurrently.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    Dolt,
    Celler,
    Compiler,
}

impl Backend {
    const fn directory(self) -> &'static str {
        match self {
            Self::Dolt => "dolt",
            Self::Celler => "celler",
            Self::Compiler => "compiler",
        }
    }
}

/// Host-private authority and service-visible data have disjoint roots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    state: PathBuf,
    cache: PathBuf,
}

impl Layout {
    /// Bases are host-selected platform directories; construction has no filesystem effects.
    pub fn new(state_base: &Path, cache_base: &Path) -> Result<Self, Error> {
        for base in [state_base, cache_base] {
            if !base.is_absolute()
                || base.components().collect::<PathBuf>().as_os_str() != base.as_os_str()
                || base
                    .components()
                    .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
                || base.as_os_str().as_encoded_bytes().contains(&0)
            {
                return Err(Error::Path);
            }
        }
        Ok(Self {
            state: state_base.join("wrix"),
            cache: cache_base.join("wrix"),
        })
    }

    pub fn control_root(&self) -> PathBuf {
        self.state.join("control")
    }

    /// Generation records belong to host control, never the backend serving mounts.
    pub fn generation_root(&self, generation: Generation) -> PathBuf {
        self.control_root()
            .join("generations")
            .join(generation.number().to_string())
    }

    pub fn backend_root(&self, backend: Backend) -> PathBuf {
        self.state.join("service-data").join(backend.directory())
    }

    /// Pure planning, not authority to mount or open the service-owned installation.
    pub fn nix_pool(&self, pool: &wrix_core::nix_store::pool::Key) -> PathBuf {
        self.state
            .join("service-data/nix")
            .join(pool.repository.to_string())
            .join(pool.system.as_str())
    }

    pub fn workspace(&self, namespace: Namespace) -> Workspace {
        let hash = String::from(namespace.clone());
        Workspace {
            namespace,
            control: self.state.join("workspaces").join(&hash),
            data: self.state.join("service-data/workspaces").join(&hash),
            cache: self
                .cache
                .join("workspaces")
                .join(&hash)
                .join("binary-cache"),
        }
    }
}

/// Canonical namespace paths contain no runtime container identity or generation selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    namespace: Namespace,
    control: PathBuf,
    data: PathBuf,
    cache: PathBuf,
}

impl Workspace {
    pub const fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    pub fn control_root(&self) -> &Path {
        &self.control
    }

    pub fn data_root(&self) -> &Path {
        &self.data
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache
    }

    pub fn services_path(&self) -> PathBuf {
        self.control.join("services.json")
    }

    pub fn cache_lock_path(&self) -> PathBuf {
        self.control.join("cache.lock")
    }

    pub fn sync_lock_path(&self) -> PathBuf {
        self.control.join("beads-sync.lock")
    }

    pub fn dolt_root(&self) -> PathBuf {
        self.data.join("dolt")
    }
}
