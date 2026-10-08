//! Typed, read-only repository overrides from the repository-root `wrix.toml`.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use displaydoc::Display;
use serde::{Deserialize, Deserializer, de};
use thiserror::Error;

use crate::{deploy_key, git::remote};

/// Repository overrides; absent fields leave invocation-specific defaults to the caller.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Policy {
    pub git: Git,
    pub init: Init,
}

/// Key identity and independent credential grants; identity alone grants nothing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Git {
    pub deploy_key: Option<deploy_key::Name>,
    pub deploy: Option<bool>,
    pub sign: Option<bool>,
    pub remote: Option<remote::Name>,
}

impl Git {
    /// Whether repository policy grants the deploy key, defaulting to false.
    pub fn deploy_enabled(&self) -> bool {
        self.deploy.unwrap_or(false)
    }

    /// Whether repository policy grants signing, defaulting to false.
    pub fn sign_enabled(&self) -> bool {
        self.sign.unwrap_or(false)
    }
}

/// Init overrides; omission preserves the caller's hook and verification defaults.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Init {
    pub prek_hooks: Option<bool>,
    pub online_verify: Option<bool>,
}

#[derive(Debug, Display, Error)]
pub enum ParseError {
    /// invalid repository policy TOML: {0}
    Toml(#[from] toml::de::Error),
    /// invalid `wrix.git.deploy_key`: {0}
    DeployKey(#[from] deploy_key::ParseError),
    /// invalid `wrix.git.remote`: {0}
    Remote(#[from] remote::ParseError),
}

#[derive(Debug, Display, Error)]
pub enum ReadError {
    /// cannot read repository policy at {path}: {source}
    Io { path: PathBuf, source: io::Error },
    /// invalid repository policy at {path}: {source}
    Policy { path: PathBuf, source: ParseError },
}

/// Read only this repository root's policy; a missing file yields no overrides.
pub fn read(repository_root: &Path) -> Result<Policy, ReadError> {
    let path = repository_root.join("wrix.toml");
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Policy::default()),
        Err(source) => return Err(ReadError::Io { path, source }),
    };
    parse(&content).map_err(|source| ReadError::Policy { path, source })
}

/// Parse overrides, accepting unrelated root extensions but rejecting unknown Wrix fields.
pub fn parse(content: &str) -> Result<Policy, ParseError> {
    let raw: RawFile = toml::from_str(content)?;
    let deploy_key = raw
        .wrix
        .git
        .deploy_key
        .as_deref()
        .map(deploy_key::Name::parse)
        .transpose()?;
    let remote = raw
        .wrix
        .git
        .remote
        .as_deref()
        .map(remote::Name::parse)
        .transpose()?;
    Ok(Policy {
        git: Git {
            deploy_key,
            deploy: raw.wrix.git.deploy,
            sign: raw.wrix.git.sign,
            remote,
        },
        init: Init {
            prek_hooks: raw.wrix.init.prek_hooks,
            online_verify: raw.wrix.init.online_verify,
        },
    })
}

fn deserialize_table<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    toml::Table::deserialize(deserializer)?
        .try_into()
        .map_err(de::Error::custom)
}

#[derive(Default, Deserialize)]
struct RawFile {
    #[serde(default, deserialize_with = "deserialize_table")]
    wrix: RawWrix,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWrix {
    #[serde(default, deserialize_with = "deserialize_table")]
    git: RawGit,
    #[serde(default, deserialize_with = "deserialize_table")]
    init: RawInit,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGit {
    deploy_key: Option<String>,
    deploy: Option<bool>,
    sign: Option<bool>,
    remote: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInit {
    prek_hooks: Option<bool>,
    online_verify: Option<bool>,
}
