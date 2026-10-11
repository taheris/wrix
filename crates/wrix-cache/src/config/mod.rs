//! Resolved cache settings shared by commands, services, and immutable hook snapshots.

use std::{collections::BTreeMap, env, fs, io, path::Path};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use wrix_core::{
    path::Workspace,
    repository_policy::{self, cache},
};

#[derive(Debug, Display, Error)]
pub enum ReadError {
    /// cache configuration I/O failed: {0}
    Io(#[from] io::Error),
    /// {0}
    Policy(#[from] repository_policy::ReadError),
    /// invalid cache configuration JSON: {0}
    Json(#[from] serde_json::Error),
    /// environment variable {name} must be valid Unicode
    Unicode { name: &'static str },
    /// environment variable {name} has invalid value {value}
    Environment { name: &'static str, value: String },
    /// cache.{name} overflows milliseconds
    Overflow { name: &'static str },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub enable: bool,
    pub require_trusted_nix: bool,
    pub publish: Roots,
    pub warm: Roots,
    pub soft_limit_bytes: u64,
    pub pending_retention_secs: u64,
    pub prune_interval_secs: u64,
    pub lock_timeout_ms: u64,
    pub explicit_lock_timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Roots {
    pub packages: bool,
    pub checks: bool,
    pub dev_shell: bool,
    pub include_roots: Vec<String>,
    pub exclude_roots: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        let publish = Roots {
            packages: true,
            checks: true,
            dev_shell: true,
            include_roots: Vec::new(),
            exclude_roots: Vec::new(),
        };
        Self {
            enable: true,
            require_trusted_nix: true,
            warm: Roots {
                checks: false,
                ..publish.clone()
            },
            publish,
            soft_limit_bytes: 50 * 1024 * 1024 * 1024,
            pending_retention_secs: 7 * 24 * 60 * 60,
            prune_interval_secs: 24 * 60 * 60,
            lock_timeout_ms: 30_000,
            explicit_lock_timeout_ms: 30_000,
        }
    }
}

const ENV_NAMES: &[&str] = &[
    "WRIX_NIX_CACHE_ENABLE",
    "WRIX_NIX_CACHE_REQUIRE_TRUSTED",
    "WRIX_CACHE_PUBLISH_PACKAGES",
    "WRIX_CACHE_PUBLISH_CHECKS",
    "WRIX_CACHE_PUBLISH_DEVSHELL",
    "WRIX_CACHE_PUBLISH_INCLUDE",
    "WRIX_CACHE_PUBLISH_EXCLUDE",
    "WRIX_CACHE_WARM_PACKAGES",
    "WRIX_CACHE_WARM_CHECKS",
    "WRIX_CACHE_WARM_DEVSHELL",
    "WRIX_CACHE_WARM_INCLUDE",
    "WRIX_CACHE_WARM_EXCLUDE",
    "WRIX_CACHE_SOFT_LIMIT_BYTES",
    "WRIX_CACHE_PENDING_RETENTION_SECS",
    "WRIX_CACHE_PRUNE_INTERVAL_SECS",
    "WRIX_CACHE_LOCK_TIMEOUT_MS",
    "WRIX_CACHE_EXPLICIT_LOCK_TIMEOUT_MS",
];

impl Settings {
    /// Resolve the invoked repository, independently of shared service identity.
    pub fn current() -> Result<Self, ReadError> {
        Self::for_workspace(&Workspace::from_current_dir()?)
    }

    pub fn for_workspace(workspace: &Workspace) -> Result<Self, ReadError> {
        let root = workspace
            .repository_root()
            .unwrap_or_else(|| workspace.canonical_path().to_path_buf());
        let policy = repository_policy::read(&root)?;
        Self::resolve(&policy.cache, &current_environment()?)
    }

    pub fn from_environment() -> Result<Self, ReadError> {
        Self::resolve(&cache::Policy::default(), &current_environment()?)
    }

    /// Resolve built-ins, repository policy, then explicit environment overrides.
    pub fn resolve(
        policy: &cache::Policy,
        env: &BTreeMap<String, String>,
    ) -> Result<Self, ReadError> {
        let defaults = Self::default();
        let lock_timeout_ms = number(
            env,
            "WRIX_CACHE_LOCK_TIMEOUT_MS",
            milliseconds(
                policy.lock_timeout,
                defaults.lock_timeout_ms,
                "lock_timeout",
            )?,
        )?;
        let explicit_lock_timeout_ms = number(
            env,
            "WRIX_CACHE_EXPLICIT_LOCK_TIMEOUT_MS",
            milliseconds(
                policy.explicit_lock_timeout,
                lock_timeout_ms,
                "explicit_lock_timeout",
            )?,
        )?;
        Ok(Self {
            enable: boolean(
                env,
                "WRIX_NIX_CACHE_ENABLE",
                policy.enable.unwrap_or(defaults.enable),
            )?,
            require_trusted_nix: boolean(
                env,
                "WRIX_NIX_CACHE_REQUIRE_TRUSTED",
                policy
                    .require_trusted_nix
                    .unwrap_or(defaults.require_trusted_nix),
            )?,
            publish: Roots::resolve(&policy.publish, &defaults.publish, env, false)?,
            warm: Roots::resolve(&policy.warm, &defaults.warm, env, true)?,
            soft_limit_bytes: number(
                env,
                "WRIX_CACHE_SOFT_LIMIT_BYTES",
                policy
                    .warn_size
                    .map_or(defaults.soft_limit_bytes, cache::Bytes::get),
            )?,
            pending_retention_secs: number(
                env,
                "WRIX_CACHE_PENDING_RETENTION_SECS",
                policy
                    .pending_ttl
                    .map_or(defaults.pending_retention_secs, cache::Seconds::get),
            )?,
            prune_interval_secs: number(
                env,
                "WRIX_CACHE_PRUNE_INTERVAL_SECS",
                policy
                    .prune_interval
                    .map_or(defaults.prune_interval_secs, cache::Seconds::get),
            )?,
            lock_timeout_ms,
            explicit_lock_timeout_ms,
        })
    }

    /// Read a complete snapshot without consulting repository files or ambient settings.
    pub fn read_snapshot(path: &Path) -> Result<Self, ReadError> {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
}

impl Roots {
    fn resolve(
        policy: &cache::Roots,
        defaults: &Self,
        env: &BTreeMap<String, String>,
        warm: bool,
    ) -> Result<Self, ReadError> {
        let names = if warm {
            [
                "WRIX_CACHE_WARM_PACKAGES",
                "WRIX_CACHE_WARM_CHECKS",
                "WRIX_CACHE_WARM_DEVSHELL",
                "WRIX_CACHE_WARM_INCLUDE",
                "WRIX_CACHE_WARM_EXCLUDE",
            ]
        } else {
            [
                "WRIX_CACHE_PUBLISH_PACKAGES",
                "WRIX_CACHE_PUBLISH_CHECKS",
                "WRIX_CACHE_PUBLISH_DEVSHELL",
                "WRIX_CACHE_PUBLISH_INCLUDE",
                "WRIX_CACHE_PUBLISH_EXCLUDE",
            ]
        };
        Ok(Self {
            packages: boolean(env, names[0], policy.packages.unwrap_or(defaults.packages))?,
            checks: boolean(env, names[1], policy.checks.unwrap_or(defaults.checks))?,
            dev_shell: boolean(
                env,
                names[2],
                policy.dev_shell.unwrap_or(defaults.dev_shell),
            )?,
            include_roots: lines(
                env,
                names[3],
                policy
                    .include_roots
                    .as_ref()
                    .unwrap_or(&defaults.include_roots),
            ),
            exclude_roots: lines(
                env,
                names[4],
                policy
                    .exclude_roots
                    .as_ref()
                    .unwrap_or(&defaults.exclude_roots),
            ),
        })
    }
}

fn current_environment() -> Result<BTreeMap<String, String>, ReadError> {
    let mut values = BTreeMap::new();
    for &name in ENV_NAMES {
        match env::var(name) {
            Ok(value) => {
                values.insert(name.to_owned(), value);
            }
            Err(env::VarError::NotPresent) => {}
            Err(env::VarError::NotUnicode(_)) => return Err(ReadError::Unicode { name }),
        }
    }
    Ok(values)
}

fn boolean(
    env: &BTreeMap<String, String>,
    name: &'static str,
    default: bool,
) -> Result<bool, ReadError> {
    match env.get(name).map(String::as_str) {
        None => Ok(default),
        Some("0" | "false" | "no") => Ok(false),
        Some("1" | "true" | "yes") => Ok(true),
        Some(value) => Err(ReadError::Environment {
            name,
            value: value.to_owned(),
        }),
    }
}

fn number(
    env: &BTreeMap<String, String>,
    name: &'static str,
    default: u64,
) -> Result<u64, ReadError> {
    env.get(name).map_or(Ok(default), |value| {
        value.parse().map_err(|_source| ReadError::Environment {
            name,
            value: value.clone(),
        })
    })
}

fn lines(env: &BTreeMap<String, String>, name: &str, default: &[String]) -> Vec<String> {
    env.get(name).map_or_else(
        || default.to_vec(),
        |value| {
            value
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        },
    )
}

fn milliseconds(
    value: Option<cache::Seconds>,
    default: u64,
    name: &'static str,
) -> Result<u64, ReadError> {
    value.map_or(Ok(default), |value| {
        value
            .get()
            .checked_mul(1000)
            .ok_or(ReadError::Overflow { name })
    })
}
