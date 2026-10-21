//! Passive exclusion of known shared-service consumption from effective host Nix settings.

use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{self, Read},
    net::{IpAddr, SocketAddr},
    num::NonZeroU16,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use displaydoc::Display;
use reqwest::Url;
use thiserror::Error as ThisError;
use wrix_core::{cache_key::CachePublicKey, path::WorkspaceHash};

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// cannot inspect retained shared-cache trust metadata: {0}
    Io(#[from] io::Error),
    /// retained shared-cache trust metadata is unsafe or invalid; reconcile it through explicit host management
    Metadata,
    /// shared-cache endpoint metadata is invalid
    Endpoint,
    /// host Nix inherits shared-service {0}; remove these entries from `NIX_CONFIG`/nix.conf and retry; Wrix will not rewrite operator configuration, delete imported outputs or attest their safety
    Inherited(&'static str),
}

/// Reads the native service endpoint shape; missing metadata is not disabled.
pub fn cache_endpoint(metadata: &str) -> Result<Option<SocketAddr>, Error> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Endpoint {
        host: IpAddr,
        port: NonZeroU16,
    }
    let value: serde_json::Value = serde_json::from_str(metadata).map_err(|_| Error::Endpoint)?;
    let field = value
        .pointer("/endpoints/cache_http")
        .ok_or(Error::Endpoint)?;
    if field.is_null() {
        return Ok(None);
    }
    let endpoint: Endpoint = serde_json::from_value(field.clone()).map_err(|_| Error::Endpoint)?;
    if !endpoint.host.is_loopback() {
        return Err(Error::Endpoint);
    }
    Ok(Some(SocketAddr::new(endpoint.host, endpoint.port.get())))
}

/// Non-secret, passively read host inventory. Construction never provisions or repairs state.
pub struct Inventory {
    cache_root: PathBuf,
    endpoint: Url,
    materials: HashSet<String>,
}

impl Inventory {
    pub fn read(state_root: &Path, cache_root: &Path, endpoint: &str) -> Result<Self, Error> {
        if !state_root.is_absolute() || !cache_root.is_absolute() {
            return Err(Error::Metadata);
        }
        let namespace = state_root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Error::Metadata)?;
        WorkspaceHash::parse(namespace).map_err(|_| Error::Metadata)?;
        let base = state_root.parent().ok_or(Error::Metadata)?;
        let mut materials = HashSet::new();
        for entry in fs::read_dir(base)? {
            let entry = entry?;
            let name = entry.file_name();
            if !name
                .to_str()
                .is_some_and(|name| name == "common" || WorkspaceHash::parse(name).is_ok())
            {
                continue;
            }
            let properties = fs::symlink_metadata(entry.path())?;
            if !properties.is_dir() || properties.file_type().is_symlink() {
                return Err(Error::Metadata);
            }
            let directory = entry.path().join("keys");
            match fs::symlink_metadata(&directory) {
                Ok(properties) if properties.is_dir() && !properties.file_type().is_symlink() => {}
                Ok(_) => return Err(Error::Metadata),
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            }
            let path = directory.join("cache.pub");
            let mut file = match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let properties = file.metadata()?;
            if !properties.is_file() || properties.nlink() != 1 || properties.len() > 1024 {
                return Err(Error::Metadata);
            }
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            let key = CachePublicKey::parse(text.trim()).map_err(|_| Error::Metadata)?;
            materials.insert(material(key.as_str()).to_owned());
        }
        let endpoint = Url::parse(&if endpoint.contains("://") {
            endpoint.to_owned()
        } else {
            format!("http://{endpoint}")
        })
        .map_err(|_| Error::Endpoint)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.port_or_known_default().is_none()
            || endpoint.host_str().is_none()
        {
            return Err(Error::Endpoint);
        }
        Ok(Self {
            cache_root: normalized(cache_root)?,
            endpoint,
            materials,
        })
    }

    /// Rejects known unsafe inherited settings without displaying their potentially secret values.
    pub fn check(&self, configuration: &str) -> Result<(), Error> {
        for line in configuration.lines() {
            let Some((setting, values)) = line.split_once('=') else {
                continue;
            };
            match setting.trim() {
                "substituters"
                | "extra-substituters"
                | "trusted-substituters"
                | "extra-trusted-substituters" => {
                    for value in values.split_whitespace() {
                        if self.service_url(value)? {
                            return Err(Error::Inherited("substitution authorization"));
                        }
                    }
                }
                "trusted-public-keys" | "extra-trusted-public-keys" => {
                    for value in values.split_whitespace() {
                        if let Ok(key) = CachePublicKey::parse(value) {
                            let (name, _) = key.as_str().split_once(':').ok_or(Error::Metadata)?;
                            if self.materials.contains(material(key.as_str())) || native_name(name)
                            {
                                return Err(Error::Inherited("signing-key trust"));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn service_url(&self, value: &str) -> Result<bool, Error> {
        let Ok(candidate) = Url::parse(value) else {
            return Ok(false);
        };
        if candidate.scheme() == "file" {
            let path = candidate.to_file_path().map_err(|()| Error::Metadata)?;
            return Ok(normalized(&path)?.starts_with(&self.cache_root));
        }
        Ok(matches!(candidate.scheme(), "http" | "https")
            && candidate.port_or_known_default() == self.endpoint.port_or_known_default()
            && candidate
                .host_str()
                .zip(self.endpoint.host_str())
                .is_some_and(|(candidate, selected)| {
                    candidate.eq_ignore_ascii_case(selected)
                        || (local_alias(candidate) && local_alias(selected))
                }))
    }
}

fn material(key: &str) -> &str {
    key.split_once(':').map_or(key, |(_, material)| material)
}

fn native_name(name: &str) -> bool {
    name == "wrix-common-v1"
        || name.strip_prefix("wrix-").is_some_and(|hash| {
            hash.len() == 43
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

fn local_alias(host: &str) -> bool {
    matches!(host, "localhost" | "localhost.localdomain")
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback() || address.is_unspecified())
}

fn normalized(path: &Path) -> io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "shared cache path must be absolute and normalized",
                ));
            }
            Ok(path.to_path_buf())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod test;
