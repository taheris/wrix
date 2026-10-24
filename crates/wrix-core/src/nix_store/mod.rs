pub mod binding;
pub mod pool;

use std::{fmt, num::NonZeroU32, path::Path};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Installed-store choice, independent of archive and compiler caching.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Private,
    Shared,
}

impl Mode {
    pub fn parse(value: &str) -> Result<Self, ModeParseError> {
        match value {
            "private" => Ok(Self::Private),
            "shared" => Ok(Self::Shared),
            _ => Err(ModeParseError),
        }
    }
}

#[derive(Debug, Display, Error)]
/// Nix store mode must be private or shared
pub struct ModeParseError;

/// Omission inherits; no field can name or authorize a pool.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Preference {
    #[serde(default, deserialize_with = "deserialize_mode")]
    pub mode: Option<Mode>,
}

impl Preference {
    pub fn resolve(self, repository: Self) -> Mode {
        self.mode.or(repository.mode).unwrap_or_default()
    }
}

fn deserialize_mode<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Mode>, D::Error> {
    Mode::deserialize(deserializer).map(Some)
}

/// A declared shared-store protocol version, not permission to use a pool.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Version(NonZeroU32);

impl Version {
    pub const CURRENT: Self = Self(NonZeroU32::MIN);

    /// Unknown versions remain readable in private mode but cannot admit shared work.
    pub fn require_supported(self) -> Result<(), CompatibilityError> {
        if self == Self::CURRENT {
            Ok(())
        } else {
            Err(CompatibilityError {
                actual: self,
                expected: Self::CURRENT,
            })
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Display, Error)]
/// unsupported shared Nix contract v{actual}; this client requires v{expected}
pub struct CompatibilityError {
    actual: Version,
    expected: Version,
}

/// Immutable image inputs; runtime admission also requires approval and service compatibility.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub version: Version,
    pub seed_closure: SeedClosure,
}

/// An immutable `closureInfo` path list, never a mutable database or root directory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String")]
pub struct SeedClosure(String);

impl SeedClosure {
    pub fn parse(value: &str) -> Result<Self, SeedClosureParseError> {
        let valid = value
            .strip_prefix("/nix/store/")
            .and_then(|tail| tail.strip_suffix("/store-paths"))
            .and_then(|base| base.split_once('-'))
            .is_some_and(|(hash, name)| {
                hash.len() == 32
                    && hash
                        .bytes()
                        .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&byte))
                    && !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"+-._?=".contains(&byte))
            });
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(SeedClosureParseError)
        }
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl TryFrom<String> for SeedClosure {
    type Error = SeedClosureParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

#[derive(Debug, Display, Error)]
/// `seed_closure` must name an immutable /nix/store path-list file
pub struct SeedClosureParseError;
