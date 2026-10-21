//! Supported namespace documents, distinct from service liveness and resource ownership.

pub mod cache;

use cache::{Manifest, Pending, Status};
use displaydoc::Display;
use serde::Deserialize;
use std::{net::Ipv4Addr, num::NonZeroU16};
use thiserror::Error;
use wrix_core::path::{ContainerName, WorkspaceHash};

#[derive(Debug, Display, Error)]
pub enum ParseError {
    /// malformed native namespace metadata: {0}
    Json(#[from] serde_json::Error),
    /// unsupported native namespace metadata schema or identity
    Unsupported,
}

#[derive(Clone, Debug)]
pub struct PendingName(String);

impl PendingName {
    pub fn parse(name: &str) -> Result<Self, ParseError> {
        if std::path::Path::new(name)
            .extension()
            .is_none_or(|extension| extension != "json")
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ParseError::Unsupported);
        }
        Ok(Self(name.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct ClosureName(String);

impl ClosureName {
    pub fn parse(name: &str) -> Result<Self, ParseError> {
        if name.is_empty()
            || name.len() > 1024
            || matches!(name, "." | "..")
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ParseError::Unsupported);
        }
        Ok(Self(name.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub enum Owner {
    Closure(ClosureName),
    Services,
    CacheStatus,
    Manifest,
    Pending(PendingName),
}

impl Owner {
    pub fn tag(&self) -> String {
        match self {
            Self::Closure(name) => format!("cache-closure/{}", name.as_str()),
            Self::Services => "services".into(),
            Self::CacheStatus => "cache-status".into(),
            Self::Manifest => "publish-roots".into(),
            Self::Pending(name) => format!("pending/{}", name.as_str()),
        }
    }

    pub fn parse(&self, bytes: &[u8], key: &WorkspaceHash) -> Result<(), ParseError> {
        match self {
            Self::Closure(_) => {
                let text = std::str::from_utf8(bytes).map_err(|_| ParseError::Unsupported)?;
                if text.lines().filter(|line| !line.is_empty()).any(|line| {
                    !line.starts_with("/nix/store/")
                        || line.split_whitespace().count() != 1
                        || line.split('/').any(|part| matches!(part, "." | ".."))
                }) {
                    return Err(ParseError::Unsupported);
                }
            }
            Self::Services => {
                let value: Services = serde_json::from_slice(bytes)?;
                if value.schema_version != 1
                    || value.workspace_hash != key.as_str()
                    || ContainerName::from_persisted(value.container_name).is_none()
                {
                    return Err(ParseError::Unsupported);
                }
            }
            Self::CacheStatus => {
                let _: Status = serde_json::from_slice(bytes)?;
            }
            Self::Manifest => {
                let value: Manifest = serde_json::from_slice(bytes)?;
                if value.schema_version.is_some_and(|version| version != 1)
                    || value
                        .workspace_hash
                        .as_deref()
                        .is_some_and(|value| value != key.as_str())
                {
                    return Err(ParseError::Unsupported);
                }
            }
            Self::Pending(_) => {
                let value: Pending = serde_json::from_slice(bytes)?;
                if value.created_at_epoch.is_none() {
                    return Err(ParseError::Unsupported);
                }
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct Services {
    schema_version: u8,
    workspace_hash: String,
    container_name: String,
    #[serde(rename = "endpoints")]
    _endpoints: Endpoints,
}

#[derive(Deserialize)]
struct Endpoints {
    #[serde(rename = "cache_http")]
    _cache_http: Option<Endpoint>,
    #[serde(rename = "dolt_tcp")]
    _dolt_tcp: Option<Endpoint>,
}

#[derive(Deserialize)]
struct Endpoint {
    #[serde(rename = "host")]
    _host: Ipv4Addr,
    #[serde(rename = "port")]
    _port: NonZeroU16,
}
