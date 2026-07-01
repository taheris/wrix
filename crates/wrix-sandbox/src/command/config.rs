use std::{collections::BTreeMap, fs, io, path::Path};

use displaydoc::Display;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    #[cfg_attr(
        target_os = "macos",
        expect(
            dead_code,
            reason = "the platform enum retains Linux so validation messages are shared across host builds"
        )
    )]
    Linux,
    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            dead_code,
            reason = "the platform enum retains Darwin so validation messages are shared across host builds"
        )
    )]
    Darwin,
}

impl Platform {
    #[cfg(target_os = "macos")]
    pub const CURRENT: Self = Self::Darwin;

    #[cfg(not(target_os = "macos"))]
    pub const CURRENT: Self = Self::Linux;

    pub const fn expected_source_kind(self) -> SourceKind {
        match self {
            Self::Linux => SourceKind::NixDescriptor,
            Self::Darwin => SourceKind::DockerArchive,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Linux => "Linux",
            Self::Darwin => "Darwin",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    NixDescriptor,
    DockerArchive,
}

impl SourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NixDescriptor => "nix-descriptor",
            Self::DockerArchive => "docker-archive",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Direct,
    Claude,
    Pi,
}

impl AgentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Claude => "claude",
            Self::Pi => "pi",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProfileConfig {
    pub profile: Profile,
    pub image: Image,
    pub agent: Agent,
    pub resources: Resources,
    pub security: Security,
    pub services: Services,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub mounts: Vec<ProfileMount>,
    #[serde(default)]
    pub writable_dirs: Vec<String>,
    #[serde(default)]
    pub network_allowlist: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProfileMount {
    pub source: String,
    pub dest: String,
    #[serde(default = "default_mount_mode")]
    pub mode: MountMode,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    Ro,
    Rw,
}

#[derive(Clone, Debug)]
pub struct Image {
    pub reference: String,
    pub source: String,
    pub source_kind: SourceKind,
    pub digest: String,
}

#[derive(Clone, Copy, Debug)]
pub struct Agent {
    pub kind: AgentKind,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Resources {
    #[serde(default)]
    pub cpus: Option<u32>,
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u32,
    #[serde(default = "default_pids_limit")]
    pub pids_limit: u32,
}

#[derive(Clone, Debug)]
pub struct Security {
    pub deploy_key: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct Services {
    pub nix_cache: NixCacheService,
}

#[derive(Clone, Copy, Debug)]
pub struct NixCacheService {
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SpawnConfig {
    #[serde(default)]
    pub image_ref: Option<String>,
    #[serde(default)]
    pub image_source: Option<String>,
    #[serde(default)]
    pub image_source_kind: Option<SourceKind>,
    pub workspace: String,
    pub env: Vec<[String; 2]>,
    pub agent_args: Vec<String>,
    #[serde(default)]
    pub mounts: Vec<SpawnMount>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SpawnMount {
    pub host_path: String,
    pub container_path: String,
    pub read_only: bool,
}

#[expect(
    clippy::doc_markdown,
    reason = "displaydoc comments are user-facing CLI errors and must not add Markdown backticks"
)]
#[derive(Debug, Display, Error)]
pub enum ConfigError {
    /// profile config not found: {path}
    MissingProfileConfig { path: String },
    /// invalid ProfileConfig JSON: {path}: {source}
    InvalidProfileConfigJson {
        path: String,
        source: serde_json::Error,
    },
    /// unsupported ProfileConfig schema: {schema}
    UnsupportedProfileConfigSchema { schema: i64 },
    /// ProfileConfig schema must be 1
    MissingProfileConfigSchema,
    /// ProfileConfig image.ref must be a non-empty string
    MissingImageRef,
    /// ProfileConfig image.source must be a non-empty string
    MissingImageSource,
    /// ProfileConfig image.source_kind must be {expected} on {platform}
    MissingImageSourceKind {
        expected: &'static str,
        platform: &'static str,
    },
    /// ProfileConfig image.source_kind must be {expected} on {platform}
    IncompatibleImageSourceKind {
        expected: &'static str,
        platform: &'static str,
    },
    /// ProfileConfig agent.kind must be direct, claude, or pi
    MissingAgentKind,
    /// invalid ProfileConfig schema: {source}
    InvalidProfileConfigSchemaShape { source: serde_json::Error },
    /// spawn-config file not found: {path}
    MissingSpawnConfig { path: String },
    /// invalid SpawnConfig JSON: {path}: {source}
    InvalidSpawnConfigJson {
        path: String,
        source: serde_json::Error,
    },
    /// invalid SpawnConfig schema: expected workspace string, optional image_ref/image_source/image_source_kind strings, env [key,value] string pairs, agent_args strings, and mounts with host_path/container_path/read_only
    InvalidSpawnConfigSchema,
    /// SpawnConfig cannot change the ProfileConfig agent/profile/image-agent field: {field}
    SpawnConfigProfileOverride { field: String },
    /// SpawnConfig field {field} is not a documented per-launch override; use a matching ProfileConfig image
    SpawnConfigDigestOverride { field: String },
    /// SpawnConfig image_source requires image_source_kind
    SpawnConfigSourceKindRequired,
    /// SpawnConfig image_source_kind must be {expected} on {platform}
    SpawnConfigIncompatibleSourceKind {
        expected: &'static str,
        platform: &'static str,
    },
    /// {source}
    Io { source: io::Error },
}

impl From<io::Error> for ConfigError {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

pub fn load_profile_config(path: &Path, platform: Platform) -> Result<ProfileConfig, ConfigError> {
    if !path.is_file() {
        return Err(ConfigError::MissingProfileConfig {
            path: path.display().to_string(),
        });
    }
    let content = fs::read_to_string(path)?;
    let value = serde_json::from_str::<Value>(&content).map_err(|source| {
        ConfigError::InvalidProfileConfigJson {
            path: path.display().to_string(),
            source,
        }
    })?;
    parse_profile_value(value, platform)
}

pub fn load_spawn_config(path: &Path, platform: Platform) -> Result<SpawnConfig, ConfigError> {
    if !path.is_file() {
        return Err(ConfigError::MissingSpawnConfig {
            path: path.display().to_string(),
        });
    }
    let content = fs::read_to_string(path)?;
    let value = serde_json::from_str::<Value>(&content).map_err(|source| {
        ConfigError::InvalidSpawnConfigJson {
            path: path.display().to_string(),
            source,
        }
    })?;
    parse_spawn_value(value, platform)
}

fn parse_profile_value(value: Value, platform: Platform) -> Result<ProfileConfig, ConfigError> {
    let schema = value
        .get("schema")
        .and_then(Value::as_i64)
        .ok_or(ConfigError::MissingProfileConfigSchema)?;
    if schema != 1 {
        return Err(ConfigError::UnsupportedProfileConfigSchema { schema });
    }

    let expected = platform.expected_source_kind();
    let image = value.get("image").and_then(Value::as_object);
    let reference = image
        .and_then(|fields| fields.get("ref"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ConfigError::MissingImageRef)?
        .to_owned();
    let source = image
        .and_then(|fields| fields.get("source"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ConfigError::MissingImageSource)?
        .to_owned();
    let source_kind = image
        .and_then(|fields| fields.get("source_kind"))
        .cloned()
        .ok_or_else(|| ConfigError::MissingImageSourceKind {
            expected: expected.as_str(),
            platform: platform.label(),
        })?;
    let source_kind = serde_json::from_value::<SourceKind>(source_kind).map_err(|_source| {
        ConfigError::IncompatibleImageSourceKind {
            expected: expected.as_str(),
            platform: platform.label(),
        }
    })?;
    if source_kind != expected {
        return Err(ConfigError::IncompatibleImageSourceKind {
            expected: expected.as_str(),
            platform: platform.label(),
        });
    }

    let agent_kind = value
        .get("agent")
        .and_then(Value::as_object)
        .and_then(|fields| fields.get("kind"))
        .cloned()
        .ok_or(ConfigError::MissingAgentKind)?;
    let kind = serde_json::from_value::<AgentKind>(agent_kind)
        .map_err(|_source| ConfigError::MissingAgentKind)?;

    let raw = serde_json::from_value::<RawProfileConfig>(value)
        .map_err(|source| ConfigError::InvalidProfileConfigSchemaShape { source })?;
    let digest = raw.image.digest.unwrap_or_default();
    Ok(ProfileConfig {
        profile: raw.profile,
        image: Image {
            reference,
            source,
            source_kind,
            digest,
        },
        agent: Agent { kind },
        resources: raw.resources.unwrap_or_default(),
        security: Security {
            deploy_key: raw
                .security
                .and_then(|security| security.deploy_key)
                .filter(|value| !value.is_empty()),
        },
        services: Services {
            nix_cache: NixCacheService {
                enabled: raw
                    .services
                    .and_then(|services| services.nix_cache)
                    .and_then(|service| service.enable)
                    .unwrap_or(true),
            },
        },
    })
}

fn parse_spawn_value(value: Value, platform: Platform) -> Result<SpawnConfig, ConfigError> {
    if !value.is_object() {
        return Err(ConfigError::InvalidSpawnConfigSchema);
    }
    if let Some(field) = first_present_field(
        &value,
        &[
            "agent",
            "agent_kind",
            "wrix_agent",
            "WRIX_AGENT",
            "profile",
            "profile_name",
            "profile_config",
            "image_agent",
            "image-agent",
        ],
    ) {
        return Err(ConfigError::SpawnConfigProfileOverride { field });
    }
    if let Some(field) = first_present_field(&value, &["image_digest", "image_digest_path"]) {
        return Err(ConfigError::SpawnConfigDigestOverride { field });
    }

    let spawn = serde_json::from_value::<SpawnConfig>(value)
        .map_err(|_source| ConfigError::InvalidSpawnConfigSchema)?;
    if spawn.workspace.is_empty()
        || spawn.env.iter().any(|pair| pair[0].is_empty())
        || spawn
            .mounts
            .iter()
            .any(|mount| mount.host_path.is_empty() || mount.container_path.is_empty())
    {
        return Err(ConfigError::InvalidSpawnConfigSchema);
    }
    if spawn
        .image_source
        .as_deref()
        .is_some_and(|source| !source.is_empty())
        && spawn.image_source_kind.is_none()
    {
        return Err(ConfigError::SpawnConfigSourceKindRequired);
    }
    if let Some(kind) = spawn.image_source_kind {
        let expected = platform.expected_source_kind();
        if kind != expected {
            return Err(ConfigError::SpawnConfigIncompatibleSourceKind {
                expected: expected.as_str(),
                platform: platform.label(),
            });
        }
    }
    Ok(spawn)
}

fn first_present_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find(|field| value.get(**field).is_some())
        .map(|field| (*field).to_owned())
}

#[derive(Debug, Deserialize)]
struct RawProfileConfig {
    profile: Profile,
    image: RawImage,
    #[serde(default)]
    resources: Option<Resources>,
    #[serde(default)]
    security: Option<RawSecurity>,
    #[serde(default)]
    services: Option<RawServices>,
}

#[derive(Debug, Deserialize)]
struct RawImage {
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSecurity {
    #[serde(default)]
    deploy_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawServices {
    #[serde(default)]
    nix_cache: Option<RawNixCacheService>,
}

#[derive(Debug, Deserialize)]
struct RawNixCacheService {
    #[serde(default)]
    enable: Option<bool>,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            cpus: None,
            memory_mb: default_memory_mb(),
            pids_limit: default_pids_limit(),
        }
    }
}

const fn default_mount_mode() -> MountMode {
    MountMode::Ro
}

const fn default_memory_mb() -> u32 {
    4096
}

const fn default_pids_limit() -> u32 {
    4096
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use super::{
        AgentKind, ConfigError, Platform, SourceKind, parse_profile_value, parse_spawn_value,
    };

    #[test]
    fn profile_config_rejects_missing_source_kind_with_field_name() {
        let value = json!({
            "schema": 1,
            "profile": { "name": "base" },
            "image": { "ref": "wrix:test", "source": "/nix/store/fake" },
            "agent": { "kind": "direct" }
        });
        let error = parse_profile_value(value, Platform::Linux).unwrap_err();
        assert!(matches!(error, ConfigError::MissingImageSourceKind { .. }));
        assert!(error.to_string().contains("image.source_kind"));
    }

    #[test]
    fn spawn_config_keeps_consumer_fields_but_rejects_agent_override() {
        let value = json!({
            "workspace": "/workspace",
            "env": [],
            "agent_args": [],
            "mounts": [],
            "initial_prompt": "consumer field"
        });
        let spawn = parse_spawn_value(value, Platform::Linux).unwrap();
        assert_eq!(spawn.workspace, "/workspace");

        let value = json!({
            "workspace": "/workspace",
            "env": [],
            "agent_args": [],
            "mounts": [],
            "agent": { "kind": "pi" }
        });
        let error = parse_spawn_value(value, Platform::Linux).unwrap_err();
        assert!(matches!(
            error,
            ConfigError::SpawnConfigProfileOverride { .. }
        ));
    }

    #[test]
    fn source_kind_and_agent_parse_as_closed_sets() {
        let source = serde_json::from_str::<SourceKind>("\"nix-descriptor\"").unwrap();
        let agent = serde_json::from_str::<AgentKind>("\"pi\"").unwrap();
        assert_eq!(source, SourceKind::NixDescriptor);
        assert_eq!(agent, AgentKind::Pi);
    }

    #[test]
    fn profile_config_parses_profile_env_and_deploy_key() {
        let value = json!({
            "schema": 1,
            "profile": { "name": "base", "env": { "FOO": "bar" } },
            "image": {
                "ref": "wrix:test",
                "source": "/nix/store/fake",
                "source_kind": "nix-descriptor"
            },
            "agent": { "kind": "direct" },
            "security": { "deploy_key": "repo-key" }
        });
        let config = parse_profile_value(value, Platform::Linux).unwrap();
        assert_eq!(config.profile.env.get("FOO"), Some(&String::from("bar")));
        assert_eq!(config.security.deploy_key, Some(String::from("repo-key")));
    }
}
