//! Worker-only cache backing and read-only launch selection.

#[cfg(test)]
mod test;

use wrix_core::{repository_policy::Policy, tool_cache::Mode};

use std::{
    env, fs, io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use super::{Kind, LaunchError, MountMode, NetworkMode, RenderedMount, Request, Staging};
use crate::command::config::ManagedCache;

const GUEST_ROOT: &str = "/mnt/wrix/tool-caches";

/// Shared sccache belongs to the service, never to independent worker daemons.
pub(super) fn prepare(
    caches: &[ManagedCache],
    mode: Mode,
    staging: &Staging,
) -> Result<Vec<RenderedMount>, LaunchError> {
    if caches.is_empty()
        || (mode == Mode::Shared && caches.iter().all(|cache| *cache == ManagedCache::Sccache))
    {
        return Ok(Vec::new());
    }
    let root = match mode {
        Mode::Shared => {
            let base = env::var_os("XDG_CACHE_HOME")
                .map_or_else(|| super::home_dir().join(".cache"), PathBuf::from);
            if !base.is_absolute() {
                return Err(io::Error::other("managed cache root must be absolute").into());
            }
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&base)?;
            base.canonicalize()?.join("wrix/tool-caches/v1")
        }
        Mode::Private => staging.root.canonicalize()?.join("tool-caches"),
    };
    let mut mounts = Vec::new();
    for cache in caches {
        let name = match cache {
            ManagedCache::Cargo => "cargo",
            ManagedCache::Uv => "uv",
            ManagedCache::Sccache if mode == Mode::Private => "sccache",
            ManagedCache::Sccache => continue,
        };
        let host = root.join(name);
        directory(&host)?;
        if *cache == ManagedCache::Cargo {
            for name in ["registry", "git"] {
                directory(&host.join(name))?;
            }
            for name in [".package-cache", ".package-cache-mutate", ".global-cache"] {
                coordination_file(&host.join(name))?;
            }
        }
        mounts.push(RenderedMount {
            host: host.display().to_string(),
            container: format!("{GUEST_ROOT}/{name}"),
            mode: MountMode::Rw,
            optional: false,
        });
    }
    Ok(mounts)
}

fn directory(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::other("managed cache root must be absolute"));
    }
    if let Some(parent) = path.parent().filter(|parent| *parent != path) {
        directory(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path)?.file_type().is_dir() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "managed cache directory is not a directory: {}",
                    path.display()
                )))
            }
        }
        Err(error) => Err(error),
    }
}

fn coordination_file(path: &Path) -> io::Result<()> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path)?.file_type().is_file() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "managed Cargo coordination file is not a regular file: {}",
                    path.display()
                )))
            }
        }
        Err(error) => Err(error),
    }
}

pub(super) fn resolve(request: &Request, policy: &Policy) -> Mode {
    let explicit = match &request.kind {
        Kind::Run(run) => run.tool_caches,
        Kind::Spawn(spawn) => spawn.config.tool_caches,
    };
    Mode::resolve(explicit, policy.tool_caches)
}

pub(super) fn restricted_conflict(request: &Request, network: NetworkMode, mode: Mode) -> bool {
    let profile = &request.profile_config.profile;
    let rust = profile.name.to_string() == "rust"
        || profile.env.keys().any(|name| name.as_str() == "RUSTC");
    rust && network == NetworkMode::Limit && mode == Mode::Shared
}
