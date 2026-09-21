use std::{collections::BTreeMap, fmt, fs, io, path::Path};

use displaydoc::Display;
use serde::{Deserialize, Deserializer, de};
use serde_json::Value;
use thiserror::Error;
use wrix_core::deploy_key::{Name as KeyName, ParseError as KeyNameParseError};

use crate::image::{Digest, ImageRef, ImageRefParseError, Source, SourceKind, SourceParseError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Linux,
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
    pub network: Network,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Network {
    pub default_mode: NetworkMode,
    pub ipv6: Ipv6Policy,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Ipv6Policy {
    #[default]
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    #[default]
    Open,
    Limit,
}

impl NetworkMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Limit => "limit",
        }
    }

    pub const fn parse(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"open" => Some(Self::Open),
            b"limit" => Some(Self::Limit),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Profile {
    pub name: ProfileName,
    #[serde(default)]
    pub env: BTreeMap<EnvName, String>,
    #[serde(default)]
    pub mounts: Vec<ProfileMount>,
    #[serde(default)]
    pub writable_dirs: Vec<String>,
    #[serde(default)]
    pub network_allowlist: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileName(String);

impl ProfileName {
    pub fn parse(value: &str) -> Result<Self, ProfileNameParseError> {
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            return Err(ProfileNameParseError {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProfileName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Display, Error)]
/// invalid profile name: {value}
pub struct ProfileNameParseError {
    value: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProfileMount {
    pub source: String,
    pub dest: String,
    #[serde(default = "default_mount_mode")]
    pub mode: MountMode,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    Ro,
    Rw,
}

#[derive(Clone, Debug)]
pub struct Image {
    pub reference: ImageRef,
    pub source: Source,
    pub digest: Option<Digest>,
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
    pub deploy_key: Option<KeyName>,
    pub runtime_secrets: BTreeMap<EnvName, RuntimeSecretPolicy>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EnvName(String);

impl EnvName {
    pub fn parse(value: &str) -> Result<Self, EnvNameParseError> {
        if is_valid_env_name(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(EnvNameParseError {
                value: value.to_owned(),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EnvName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EnvName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        Self::parse(&name).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Display, Error)]
/// invalid environment variable name: {value}
pub struct EnvNameParseError {
    value: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeSecretPolicy {
    Optional,
    Required,
}

#[derive(Clone, Copy, Debug)]
pub struct Services {
    pub nix_cache: NixCacheService,
}

#[derive(Clone, Copy, Debug)]
pub struct NixCacheService {
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub struct SpawnConfig {
    pub image_ref: Option<ImageRef>,
    pub image_source: Option<Source>,
    pub workspace: String,
    pub env: Vec<(EnvName, String)>,
    pub agent_args: Vec<String>,
    pub mounts: Vec<SpawnMount>,
}

#[derive(Debug, Deserialize)]
struct RawSpawnConfig {
    image_ref: Option<ImageRef>,
    image_source: Option<String>,
    image_source_kind: Option<SourceKind>,
    workspace: String,
    env: Vec<(EnvName, String)>,
    agent_args: Vec<String>,
    #[serde(default)]
    mounts: Vec<SpawnMount>,
    #[serde(flatten)]
    extensions: BTreeMap<String, Value>,
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
    /// ProfileConfig profile.name must be a non-empty identifier
    MissingProfileName,
    /// {source}
    InvalidProfileName { source: ProfileNameParseError },
    /// ProfileConfig image.ref must be a non-empty string
    MissingImageRef,
    /// {source}
    InvalidImageRef { source: ImageRefParseError },
    /// ProfileConfig image.source must be a non-empty string
    MissingImageSource,
    /// invalid image source path: {source}
    InvalidImageSource { source: SourceParseError },
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
    /// ProfileConfig profile.env cannot contain runtime credential {name}
    StaticCredentialInProfileEnv { name: EnvName },
    /// ProfileConfig profile.env cannot set sandbox bootstrap environment variable {name}
    BootstrapEnvironmentInProfileEnv { name: EnvName },
    /// ProfileConfig security.runtime_secrets cannot declare sandbox bootstrap environment variable {name}
    BootstrapEnvironmentInRuntimeSecrets { name: EnvName },
    /// invalid ProfileConfig security.deploy_key: {source}
    InvalidDeployKeyName { source: KeyNameParseError },
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
    /// SpawnConfig.env cannot set sandbox bootstrap environment variable {name}
    BootstrapEnvironmentInSpawnEnv { name: EnvName },
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
    let raw = serde_json::from_str::<RawProfileConfig>(&content).map_err(|source| {
        if source.is_data() {
            ConfigError::InvalidProfileConfigSchemaShape { source }
        } else {
            ConfigError::InvalidProfileConfigJson {
                path: path.display().to_string(),
                source,
            }
        }
    })?;
    raw.into_config(platform)
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

#[cfg(test)]
fn parse_profile_value(value: Value, platform: Platform) -> Result<ProfileConfig, ConfigError> {
    serde_json::from_value::<RawProfileConfig>(value)
        .map_err(|source| ConfigError::InvalidProfileConfigSchemaShape { source })?
        .into_config(platform)
}

impl RawProfileConfig {
    fn into_config(self, platform: Platform) -> Result<ProfileConfig, ConfigError> {
        let schema = self.schema.ok_or(ConfigError::MissingProfileConfigSchema)?;
        if schema != 1 {
            return Err(ConfigError::UnsupportedProfileConfigSchema { schema });
        }
        let raw_profile = self.profile.ok_or(ConfigError::MissingProfileName)?;
        let name = raw_profile
            .name
            .filter(|name| !name.is_empty())
            .ok_or(ConfigError::MissingProfileName)?;
        let profile = Profile {
            name: ProfileName::parse(&name)
                .map_err(|source| ConfigError::InvalidProfileName { source })?,
            env: raw_profile.env,
            mounts: raw_profile.mounts,
            writable_dirs: raw_profile.writable_dirs,
            network_allowlist: raw_profile.network_allowlist,
        };
        let image = self.image.unwrap_or_default();
        let reference = image
            .reference
            .filter(|value| !value.is_empty())
            .ok_or(ConfigError::MissingImageRef)?;
        let reference = ImageRef::parse(&reference)
            .map_err(|source| ConfigError::InvalidImageRef { source })?;
        let source = image
            .source
            .filter(|value| !value.is_empty())
            .ok_or(ConfigError::MissingImageSource)?;
        let expected = platform.expected_source_kind();
        let source_kind = image
            .source_kind
            .ok_or_else(|| ConfigError::MissingImageSourceKind {
                expected: expected.as_str(),
                platform: platform.label(),
            })?;
        if source_kind != expected {
            return Err(ConfigError::IncompatibleImageSourceKind {
                expected: expected.as_str(),
                platform: platform.label(),
            });
        }
        let kind = self
            .agent
            .and_then(|agent| agent.kind)
            .ok_or(ConfigError::MissingAgentKind)?;
        let RawSecurity {
            deploy_key,
            runtime_secrets,
        } = self.security.unwrap_or_default();
        let deploy_key = deploy_key
            .map(|value| {
                KeyName::parse(&value)
                    .map_err(|source| ConfigError::InvalidDeployKeyName { source })
            })
            .transpose()?;
        if let Some(name) = profile.env.keys().find(|name| {
            is_known_credential_env(name.as_str()) || runtime_secrets.contains_key(*name)
        }) {
            return Err(ConfigError::StaticCredentialInProfileEnv { name: name.clone() });
        }
        if let Some(name) = profile
            .env
            .keys()
            .find(|name| is_bootstrap_sensitive_env(name.as_str()))
        {
            return Err(ConfigError::BootstrapEnvironmentInProfileEnv { name: name.clone() });
        }
        if let Some(name) = runtime_secrets
            .keys()
            .find(|name| is_bootstrap_sensitive_env(name.as_str()))
        {
            return Err(ConfigError::BootstrapEnvironmentInRuntimeSecrets { name: name.clone() });
        }
        Ok(ProfileConfig {
            profile,
            network: self.network,
            image: Image {
                reference,
                source: Source::parse(source, source_kind)
                    .map_err(|source| ConfigError::InvalidImageSource { source })?,
                digest: image.digest,
            },
            agent: Agent { kind },
            resources: self.resources.unwrap_or_default(),
            security: Security {
                deploy_key,
                runtime_secrets,
            },
            services: Services {
                nix_cache: NixCacheService {
                    enabled: self
                        .services
                        .and_then(|services| services.nix_cache)
                        .and_then(|service| service.enable)
                        .unwrap_or(true),
                },
            },
        })
    }
}

fn parse_spawn_value(value: Value, platform: Platform) -> Result<SpawnConfig, ConfigError> {
    let spawn = serde_json::from_value::<RawSpawnConfig>(value)
        .map_err(|_source| ConfigError::InvalidSpawnConfigSchema)?;
    if let Some(field) = first_present_field(
        &spawn.extensions,
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
    if let Some(field) =
        first_present_field(&spawn.extensions, &["image_digest", "image_digest_path"])
    {
        return Err(ConfigError::SpawnConfigDigestOverride { field });
    }

    if let Some((name, _value)) = spawn
        .env
        .iter()
        .find(|(name, _value)| is_bootstrap_sensitive_env(name.as_str()))
    {
        return Err(ConfigError::BootstrapEnvironmentInSpawnEnv { name: name.clone() });
    }
    if spawn.workspace.is_empty()
        || spawn
            .mounts
            .iter()
            .any(|mount| mount.host_path.is_empty() || mount.container_path.is_empty())
    {
        return Err(ConfigError::InvalidSpawnConfigSchema);
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
    let image_source = spawn
        .image_source
        .filter(|source| !source.is_empty())
        .map(|source| {
            let kind = spawn
                .image_source_kind
                .ok_or(ConfigError::SpawnConfigSourceKindRequired)?;
            Source::parse(source, kind).map_err(|source| ConfigError::InvalidImageSource { source })
        })
        .transpose()?;
    Ok(SpawnConfig {
        image_ref: spawn.image_ref,
        image_source,
        workspace: spawn.workspace,
        env: spawn.env,
        agent_args: spawn.agent_args,
        mounts: spawn.mounts,
    })
}

fn first_present_field(value: &BTreeMap<String, Value>, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find(|field| value.get(**field).is_some())
        .map(|field| (*field).to_owned())
}

#[derive(Debug, Deserialize)]
struct RawProfileConfig {
    schema: Option<i64>,
    #[serde(default)]
    network: Network,
    profile: Option<RawProfile>,
    image: Option<RawImage>,
    agent: Option<RawAgent>,
    #[serde(default)]
    resources: Option<Resources>,
    #[serde(default)]
    security: Option<RawSecurity>,
    #[serde(default)]
    services: Option<RawServices>,
}

#[derive(Debug, Deserialize)]
struct RawProfile {
    name: Option<String>,
    #[serde(default)]
    env: BTreeMap<EnvName, String>,
    #[serde(default)]
    mounts: Vec<ProfileMount>,
    #[serde(default)]
    writable_dirs: Vec<String>,
    #[serde(default)]
    network_allowlist: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawImage {
    #[serde(rename = "ref")]
    reference: Option<String>,
    source: Option<String>,
    source_kind: Option<SourceKind>,
    digest: Option<Digest>,
}

#[derive(Debug, Deserialize)]
struct RawAgent {
    kind: Option<AgentKind>,
}

#[derive(Debug, Default, Deserialize)]
struct RawSecurity {
    #[serde(default)]
    deploy_key: Option<String>,
    #[serde(default)]
    runtime_secrets: BTreeMap<EnvName, RuntimeSecretPolicy>,
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

pub fn is_known_credential_env(name: &str) -> bool {
    matches!(
        name,
        "ANTHROPIC_API_KEY" | "CLAUDE_CODE_OAUTH_TOKEN" | "OPENAI_API_KEY"
    )
}

fn is_bootstrap_sensitive_env(name: &str) -> bool {
    const EXACT_NAMES: &[&str] = &[
        "BASHOPTS",
        "BASH_ENV",
        "ENV",
        "GLIBC_TUNABLES",
        "PATH",
        "PS4",
        "SHELLOPTS",
        "WRIX_FIREWALL_BACKEND",
        "WRIX_NETWORK",
        "WRIX_NOTIFY_TCP",
        "WRIX_WAIT_FOR_ROUTE",
    ];
    const PREFIXES: &[&str] = &[
        "BEADS_DOLT_SERVER_",
        "LD_",
        "WRIX_NETWORK_",
        "WRIX_NIX_CACHE_",
        "WRIX_PROJECT_CACHE_",
    ];

    EXACT_NAMES.contains(&name) || PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

fn is_valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use super::{
        AgentKind, ConfigError, Platform, ProfileName, SourceKind, is_bootstrap_sensitive_env,
        parse_profile_value, parse_spawn_value,
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
    fn profile_boundary_preserves_extension_fields_and_typed_source_semantics() {
        for (platform, kind) in [
            (Platform::Linux, "nix-descriptor"),
            (Platform::Darwin, "docker-archive"),
        ] {
            let value = json!({
                "schema":1, "system":"future-system", "future_extension":{"enabled":true},
                "profile":{"name":"base", "future_profile_field":[1,2]},
                "image":{"ref":"wrix:test", "source":"/not-yet-realized", "source_kind":kind, "future_image_field":true},
                "agent":{"kind":"pi", "future_agent_field":true},
                "services":{"beads":{"enable":"auto"}, "future_service":{}},
                "features":{"mcp_runtime":true},
                "network":{"default_mode":"limit", "future_network_field":true}
            });
            let config = parse_profile_value(value, platform).unwrap();
            assert_eq!(config.image.source.kind(), platform.expected_source_kind());
            assert_eq!(
                config.image.source.path(),
                std::path::Path::new("/not-yet-realized")
            );
            assert_eq!(config.agent.kind, AgentKind::Pi);
            assert_eq!(config.network.default_mode, super::NetworkMode::Limit);
            assert!(config.services.nix_cache.enabled);
        }
    }

    #[test]
    fn spawn_source_override_is_absent_or_a_complete_typed_source() {
        for value in [
            json!({}),
            json!({"image_source":""}),
            json!({"image_source_kind":"nix-descriptor"}),
        ] {
            let mut base = json!({"workspace":"/workspace", "env":[], "agent_args":[]});
            base.as_object_mut()
                .unwrap()
                .extend(value.as_object().unwrap().clone());
            assert!(
                parse_spawn_value(base, Platform::Linux)
                    .unwrap()
                    .image_source
                    .is_none()
            );
        }
        let spawn = parse_spawn_value(json!({"workspace":"/workspace", "env":[], "agent_args":[], "image_source":"/image", "image_source_kind":"nix-descriptor"}), Platform::Linux).unwrap();
        let source = spawn.image_source.unwrap();
        assert_eq!(source.path(), std::path::Path::new("/image"));
        assert_eq!(source.kind(), SourceKind::NixDescriptor);
    }

    #[test]
    fn source_kind_and_agent_parse_as_closed_sets() {
        let source = serde_json::from_str::<SourceKind>("\"nix-descriptor\"").unwrap();
        let agent = serde_json::from_str::<AgentKind>("\"pi\"").unwrap();
        assert_eq!(source, SourceKind::NixDescriptor);
        assert_eq!(agent, AgentKind::Pi);
    }

    #[test]
    fn profile_name_rejects_whitespace() {
        assert!(ProfileName::parse("rust profile").is_err());

        let value = json!({
            "schema": 1,
            "profile": { "name": "rust profile" },
            "image": {
                "ref": "wrix:test",
                "source": "/nix/store/fake",
                "source_kind": "nix-descriptor"
            },
            "agent": { "kind": "direct" }
        });
        assert!(matches!(
            parse_profile_value(value, Platform::Linux),
            Err(ConfigError::InvalidProfileName { .. })
        ));
    }

    #[test]
    fn image_ref_rejects_whitespace() {
        let value = json!({
            "schema": 1,
            "profile": { "name": "rust" },
            "image": {
                "ref": "wrix image:test",
                "source": "/nix/store/fake",
                "source_kind": "nix-descriptor"
            },
            "agent": { "kind": "direct" }
        });
        assert!(matches!(
            parse_profile_value(value, Platform::Linux),
            Err(ConfigError::InvalidImageRef { .. })
        ));
    }

    #[test]
    fn profile_config_parses_static_env_and_runtime_secret_policy() {
        let value = json!({
            "schema": 1,
            "profile": { "name": "base", "env": { "FOO": "bar" } },
            "image": {
                "ref": "wrix:test",
                "source": "/nix/store/fake",
                "source_kind": "nix-descriptor"
            },
            "agent": { "kind": "direct" },
            "security": {
                "deploy_key": "repo-key",
                "runtime_secrets": { "CUSTOM_TOKEN": "required" }
            }
        });
        let config = parse_profile_value(value, Platform::Linux).unwrap();
        let (name, value) = config.profile.env.first_key_value().unwrap();
        assert_eq!(name.as_str(), "FOO");
        assert_eq!(value, "bar");
        assert_eq!(
            config
                .security
                .deploy_key
                .as_ref()
                .map(super::KeyName::as_str),
            Some("repo-key")
        );
        let (name, policy) = config.security.runtime_secrets.first_key_value().unwrap();
        assert_eq!(name.as_str(), "CUSTOM_TOKEN");
        assert_eq!(*policy, super::RuntimeSecretPolicy::Required);
    }

    #[test]
    fn profile_config_rejects_unsafe_deploy_key_names() {
        for name in ["", ".", "..", "/tmp/key", "nested/key", "nested\\key"] {
            let value = json!({
                "schema": 1,
                "profile": { "name": "base" },
                "image": {
                    "ref": "wrix:test",
                    "source": "/nix/store/fake",
                    "source_kind": "nix-descriptor"
                },
                "agent": { "kind": "direct" },
                "security": { "deploy_key": name }
            });
            let error = parse_profile_value(value, Platform::Linux).unwrap_err();
            assert!(matches!(error, ConfigError::InvalidDeployKeyName { .. }));
        }
    }

    #[test]
    fn profile_config_rejects_credentials_in_static_env() {
        for (name, runtime_secrets) in [
            ("OPENAI_API_KEY", json!({})),
            ("CUSTOM_TOKEN", json!({ "CUSTOM_TOKEN": "optional" })),
        ] {
            let value = json!({
                "schema": 1,
                "profile": { "name": "base", "env": { (name): "secret" } },
                "image": {
                    "ref": "wrix:test",
                    "source": "/nix/store/fake",
                    "source_kind": "nix-descriptor"
                },
                "agent": { "kind": "direct" },
                "security": { "runtime_secrets": runtime_secrets }
            });
            let error = parse_profile_value(value, Platform::Linux).unwrap_err();
            assert!(matches!(
                error,
                ConfigError::StaticCredentialInProfileEnv { .. }
            ));
            assert!(error.to_string().contains(name));
        }
    }

    #[test]
    fn bootstrap_sensitive_environment_names_are_reserved() {
        for name in [
            "BASHOPTS",
            "BASH_ENV",
            "BEADS_DOLT_SERVER_HOST",
            "ENV",
            "GLIBC_TUNABLES",
            "LD_LIBRARY_PATH",
            "LD_PRELOAD",
            "PATH",
            "PS4",
            "SHELLOPTS",
            "WRIX_FIREWALL_BACKEND",
            "WRIX_NETWORK",
            "WRIX_NETWORK_DNS_SERVERS",
            "WRIX_NETWORK_LOCAL_ENDPOINTS",
            "WRIX_NIX_CACHE_HOST",
            "WRIX_NOTIFY_TCP",
            "WRIX_PROJECT_CACHE_PORT",
            "WRIX_WAIT_FOR_ROUTE",
        ] {
            assert!(is_bootstrap_sensitive_env(name), "{name}");
        }
        for name in [
            "APP_PATH",
            "LD",
            "WRIX_NETWORKING",
            "WRIX_NOTIFY_TCP_VERBOSE",
        ] {
            assert!(!is_bootstrap_sensitive_env(name), "{name}");
        }
    }

    #[test]
    fn config_env_surfaces_reject_bootstrap_sensitive_names() {
        let profile = json!({
            "schema": 1,
            "profile": { "name": "base", "env": { "BASH_ENV": "/workspace/bootstrap.sh" } },
            "image": {
                "ref": "wrix:test",
                "source": "/nix/store/fake",
                "source_kind": "nix-descriptor"
            },
            "agent": { "kind": "direct" }
        });
        assert!(matches!(
            parse_profile_value(profile, Platform::Linux),
            Err(ConfigError::BootstrapEnvironmentInProfileEnv { .. })
        ));

        let runtime_secret = json!({
            "schema": 1,
            "profile": { "name": "base" },
            "image": {
                "ref": "wrix:test",
                "source": "/nix/store/fake",
                "source_kind": "nix-descriptor"
            },
            "agent": { "kind": "direct" },
            "security": { "runtime_secrets": { "LD_PRELOAD": "optional" } }
        });
        assert!(matches!(
            parse_profile_value(runtime_secret, Platform::Linux),
            Err(ConfigError::BootstrapEnvironmentInRuntimeSecrets { .. })
        ));

        let spawn = json!({
            "workspace": "/workspace",
            "env": [["WRIX_NETWORK_LOCAL_ENDPOINTS", "192.168.1.2:80/tcp"]],
            "agent_args": [],
            "mounts": []
        });
        assert!(matches!(
            parse_spawn_value(spawn, Platform::Linux),
            Err(ConfigError::BootstrapEnvironmentInSpawnEnv { .. })
        ));
    }

    #[test]
    fn config_env_surfaces_reject_invalid_names() {
        let profile = json!({
            "schema": 1,
            "profile": { "name": "base", "env": { "OPENAI_API_KEY=shadow": "secret" } },
            "image": {
                "ref": "wrix:test",
                "source": "/nix/store/fake",
                "source_kind": "nix-descriptor"
            },
            "agent": { "kind": "direct" }
        });
        assert!(matches!(
            parse_profile_value(profile, Platform::Linux),
            Err(ConfigError::InvalidProfileConfigSchemaShape { .. })
        ));

        let spawn = json!({
            "workspace": "/workspace",
            "env": [["OPENAI_API_KEY=shadow", "secret"]],
            "agent_args": [],
            "mounts": []
        });
        assert!(matches!(
            parse_spawn_value(spawn, Platform::Linux),
            Err(ConfigError::InvalidSpawnConfigSchema)
        ));
    }

    #[test]
    fn profile_config_rejects_invalid_runtime_secret_name() {
        let value = json!({
            "schema": 1,
            "profile": { "name": "base" },
            "image": {
                "ref": "wrix:test",
                "source": "/nix/store/fake",
                "source_kind": "nix-descriptor"
            },
            "agent": { "kind": "direct" },
            "security": { "runtime_secrets": { "NOT-AN-ENV-NAME": "optional" } }
        });
        let error = parse_profile_value(value, Platform::Linux).unwrap_err();
        assert!(matches!(
            error,
            ConfigError::InvalidProfileConfigSchemaShape { .. }
        ));
    }

    #[test]
    fn profile_config_rejects_invalid_runtime_secret_policy() {
        let value = json!({
            "schema": 1,
            "profile": { "name": "base" },
            "image": {
                "ref": "wrix:test",
                "source": "/nix/store/fake",
                "source_kind": "nix-descriptor"
            },
            "agent": { "kind": "direct" },
            "security": { "runtime_secrets": { "CUSTOM_TOKEN": "sometimes" } }
        });
        let error = parse_profile_value(value, Platform::Linux).unwrap_err();
        assert!(matches!(
            error,
            ConfigError::InvalidProfileConfigSchemaShape { .. }
        ));
    }

    #[test]
    fn profile_config_preserves_mount_optionality() {
        let value = json!({
            "schema": 1,
            "profile": {
                "name": "base",
                "mounts": [
                    {
                        "source": "/host/optional",
                        "dest": "/container/optional",
                        "mode": "rw",
                        "optional": true
                    },
                    {
                        "source": "/host/required",
                        "dest": "/container/required"
                    }
                ]
            },
            "image": {
                "ref": "wrix:test",
                "source": "/nix/store/fake",
                "source_kind": "nix-descriptor"
            },
            "agent": { "kind": "direct" }
        });
        let config = parse_profile_value(value, Platform::Linux).unwrap();
        assert!(config.profile.mounts[0].optional);
        assert!(!config.profile.mounts[1].optional);
    }
}
