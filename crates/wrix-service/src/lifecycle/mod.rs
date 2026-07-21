use std::{
    env, fs, io,
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::{fs::FileTypeExt, net::UnixStream};

use displaydoc::Display;
use serde::{Deserialize, Deserializer, de};
use thiserror::Error as ThisError;
use wrix_core::{
    cache_key,
    path::{ContainerName, Workspace, WorkspaceHash},
};
use wrix_sandbox::image::{
    self as runtime_image, CommandStore, Digest, InstallRequest, RetentionRequest,
    Runtime as ImageRuntime, SourceKind,
};

const SCHEMA_VERSION: u8 = 1;
const CACHE_PORT_START: u16 = 21_000;
const CACHE_PORT_WIDTH: u16 = 2_000;
const DOLT_PORT_START: u16 = 23_000;
const DOLT_PORT_WIDTH: u16 = 2_000;
const CACHE_ENABLED_LABEL: &str = "wrix.cache.enabled";
const DOLT_TRANSPORT_LABEL: &str = "wrix.dolt.transport";
const DOLT_DISABLED_LABEL_VALUE: &str = "disabled";
const DOLT_READY_TIMEOUT: Duration = Duration::from_secs(6);
const DOLT_READY_INTERVAL: Duration = Duration::from_millis(200);

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// service lifecycle I/O failed
    Io {
        #[from]
        source: io::Error,
    },
    /// project cache operation failed: {source}
    Cache {
        #[from]
        source: wrix_cache::publisher::Error,
    },
    /// runtime image operation failed: {source}
    Image {
        #[from]
        source: runtime_image::Error,
    },
    /// environment variable {name} must be valid Unicode
    InvalidUnicodeEnvironment { name: &'static str },
    /// unknown Dolt transport: {value}
    UnknownDoltTransport { value: String },
    /// failed to remove stale Dolt socket {path}: {source}
    StaleDoltSocketRemoval { path: String, source: io::Error },
    /// Dolt endpoint {endpoint} did not become reachable within {timeout} seconds: {source}
    DoltEndpointUnavailable {
        endpoint: String,
        timeout: u64,
        source: io::Error,
    },
    /// unknown service image source kind: {value}
    UnknownImageSourceKind { value: String },
    /// invalid service image digest: {value}
    InvalidImageDigest { value: String },
    /// `WRIX_SERVICE_IMAGE_SOURCE_KIND` is required when `WRIX_SERVICE_IMAGE_SOURCE` is set
    MissingImageSourceKind,
    /// service image digest path does not exist: {path}
    MissingImageDigestPath { path: String },
    /// installed service image from {path}, but image {image} is unavailable
    InstalledImageUnavailable { path: String, image: String },
    /// service lifecycle operation failed: {message}
    Operation { message: String },
    /// service port {port} is already in use by {owner}; stop that process/container or free the port before retrying
    PortInUse { port: u16, owner: String },
    /// no available loopback port in {start}-{end}
    NoAvailableLoopbackPort { start: u16, end: u16 },
    /// invalid persisted service metadata at {path}: {source}
    PersistedServicesJson {
        path: String,
        source: serde_json::Error,
    },
    /// invalid persisted workspace hash: {value}
    InvalidPersistedWorkspaceHash { value: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheMode {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug)]
pub struct Plan {
    workspace: Workspace,
    paths: Paths,
    container_name: ContainerName,
    cache_port: Option<u16>,
    dolt: Option<DoltEndpoint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoltEndpoint {
    transport: DoltTransport,
    socket_path: PathBuf,
    tcp_port: Option<u16>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoltTransport {
    UnixSocket,
    Tcp,
}

#[derive(Clone, Debug)]
pub struct Paths {
    state_root: PathBuf,
    cache_root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeStatus {
    Running,
    Stopped,
    Missing,
}

#[derive(Clone, Debug)]
pub struct Status {
    pub runtime: RuntimeStatus,
    pub plan: Plan,
}

impl Plan {
    pub fn for_current_dir(cache_mode: CacheMode) -> Result<Self> {
        let workspace = Workspace::from_service_current_dir()?;
        Self::for_workspace(workspace, cache_mode)
    }

    pub fn for_workspace(workspace: Workspace, cache_mode: CacheMode) -> Result<Self> {
        let paths = Paths::for_workspace(workspace.hash())?;
        Self::for_workspace_with_paths(workspace, cache_mode, paths)
    }

    pub fn for_workspace_with_paths(
        workspace: Workspace,
        cache_mode: CacheMode,
        paths: Paths,
    ) -> Result<Self> {
        let container_name = select_container_name(&workspace, &paths)?;
        let prior_ports = PortLease::read(&paths.services_path())?;
        let cache_port = match cache_mode {
            CacheMode::Enabled if cache_allowed_for_workspace(workspace.canonical_path()) => {
                Some(select_port(
                    prior_ports.cache_http_port,
                    CACHE_PORT_START,
                    CACHE_PORT_WIDTH,
                    workspace.hash(),
                )?)
            }
            CacheMode::Enabled | CacheMode::Disabled => None,
        };
        let dolt = if workspace.canonical_path().join(".beads/dolt").is_dir() {
            let transport = DoltTransport::from_env()?;
            let tcp_port = match transport {
                DoltTransport::UnixSocket => None,
                DoltTransport::Tcp => Some(select_port(
                    prior_ports.dolt_tcp_port,
                    DOLT_PORT_START,
                    DOLT_PORT_WIDTH,
                    workspace.hash(),
                )?),
            };
            Some(DoltEndpoint {
                transport,
                socket_path: workspace.canonical_path().join(".wrix/dolt.sock"),
                tcp_port,
            })
        } else {
            None
        };
        Ok(Self {
            workspace,
            paths,
            container_name,
            cache_port,
            dolt,
        })
    }

    pub const fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub const fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn container_name(&self) -> ContainerName {
        self.container_name.clone()
    }

    pub const fn cache_port(&self) -> Option<u16> {
        self.cache_port
    }

    pub const fn dolt(&self) -> Option<&DoltEndpoint> {
        self.dolt.as_ref()
    }

    pub fn dolt_port(&self) -> Option<u16> {
        self.dolt.as_ref().and_then(DoltEndpoint::tcp_port)
    }

    const fn cache_enabled(&self) -> bool {
        self.cache_port.is_some()
    }

    const fn has_services(&self) -> bool {
        self.cache_port.is_some() || self.dolt.is_some()
    }

    fn selected_host_ports(&self) -> Vec<u16> {
        let mut ports = Vec::new();
        if let Some(port) = self.cache_port {
            ports.push(port);
        }
        if let Some(port) = self.dolt_port() {
            ports.push(port);
        }
        ports
    }

    pub fn ensure_layout(&self) -> Result<()> {
        fs::create_dir_all(self.paths.state_root())?;
        if self.cache_enabled() {
            fs::create_dir_all(self.paths.cache_root())?;
            fs::create_dir_all(self.paths.gcroots_dir())?;
            fs::create_dir_all(self.paths.keys_dir())?;
            fs::create_dir_all(self.paths.pending_dir())?;
            fs::create_dir_all(self.paths.cache_root().join("nar"))?;
            fs::create_dir_all(self.paths.cache_root().join("log"))?;
            write_if_missing(&self.paths.cache_lock_path(), "")?;
            write_if_missing(
                &self.paths.cache_status_path(),
                default_cache_status().as_bytes(),
            )?;
            self.ensure_cache_keys()?;
            write_if_missing(&self.paths.publish_roots_path(), b"{\n  \"roots\": []\n}\n")?;
            write_if_missing(
                &self.paths.cache_root().join("nix-cache-info"),
                nix_cache_info().as_bytes(),
            )?;
            wrix_cache::publisher::prune_stale_dirty(
                self.paths.state_root(),
                self.paths.cache_root(),
            )?;
        }
        if self.dolt.is_some() {
            fs::create_dir_all(self.workspace.canonical_path().join(".wrix"))?;
        }
        self.write_services()
    }

    fn beads_worktree_remote(&self) -> Option<PathBuf> {
        let path = self
            .workspace
            .canonical_path()
            .join(".git/beads-worktrees/beads/.beads/dolt-remote");
        path.is_dir().then_some(path)
    }

    fn write_services(&self) -> Result<()> {
        fs::write(self.paths.services_path(), self.services_json())?;
        Ok(())
    }

    pub fn services_json(&self) -> String {
        let cache_port = json_port(self.cache_port);
        let dolt = json_dolt_endpoint(self.dolt.as_ref());
        let dolt_unix = json_dolt_unix(self.dolt.as_ref());
        let dolt_tcp = json_dolt_tcp(self.dolt.as_ref());
        format!(
            concat!(
                "{{\n",
                "  \"schema_version\": {},\n",
                "  \"workspace_path\": \"{}\",\n",
                "  \"workspace_hash\": \"{}\",\n",
                "  \"container_name\": \"{}\",\n",
                "  \"state_root\": \"{}\",\n",
                "  \"cache_root\": \"{}\",\n",
                "  \"endpoints\": {{\n",
                "    \"cache_http\": {},\n",
                "    \"dolt\": {},\n",
                "    \"dolt_unix\": {},\n",
                "    \"dolt_tcp\": {}\n",
                "  }}\n",
                "}}\n"
            ),
            SCHEMA_VERSION,
            escape_json(&self.workspace.canonical_path().display().to_string()),
            self.workspace.hash(),
            escape_json(self.container_name().as_str()),
            escape_json(&self.paths.state_root().display().to_string()),
            escape_json(&self.paths.cache_root().display().to_string()),
            cache_port,
            dolt,
            dolt_unix,
            dolt_tcp
        )
    }

    fn ensure_cache_keys(&self) -> Result<()> {
        let key_name = format!("wrix-cache-{}", self.workspace.hash());
        let nix_store = env::var("WRIX_NIX_STORE").unwrap_or_else(|_| String::from("nix-store"));
        cache_key::ensure_keypair(
            &key_name,
            &self.paths.cache_secret_path(),
            &self.paths.cache_public_path(),
            &nix_store,
        )?;
        Ok(())
    }
}

impl DoltEndpoint {
    pub const fn transport(&self) -> DoltTransport {
        self.transport
    }

    pub fn socket_path(&self) -> &Path {
        self.socket_path.as_path()
    }

    pub const fn tcp_port(&self) -> Option<u16> {
        self.tcp_port
    }

    pub const fn tcp_host(&self) -> Option<&'static str> {
        match self.transport {
            DoltTransport::UnixSocket => None,
            DoltTransport::Tcp => Some("127.0.0.1"),
        }
    }
}

impl DoltTransport {
    fn from_env() -> Result<Self> {
        match env::var("WRIX_DOLT_TRANSPORT") {
            Ok(value) => Self::parse(&value),
            Err(env::VarError::NotPresent) => Ok(Self::platform_default()),
            Err(env::VarError::NotUnicode(_)) => Err(Error::InvalidUnicodeEnvironment {
                name: "WRIX_DOLT_TRANSPORT",
            }),
        }
    }

    fn parse(input: &str) -> Result<Self> {
        match input {
            "unix" | "socket" => Ok(Self::UnixSocket),
            "tcp" => Ok(Self::Tcp),
            other => Err(Error::UnknownDoltTransport {
                value: other.to_owned(),
            }),
        }
    }

    const fn platform_default() -> Self {
        if cfg!(target_os = "macos") {
            Self::Tcp
        } else {
            Self::UnixSocket
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnixSocket => "unix",
            Self::Tcp => "tcp",
        }
    }
}

impl Paths {
    pub fn new(state_root: impl Into<PathBuf>, cache_root: impl Into<PathBuf>) -> Self {
        Self {
            state_root: state_root.into(),
            cache_root: cache_root.into(),
        }
    }

    fn for_workspace(hash: &WorkspaceHash) -> Result<Self> {
        let home = home_dir()?;
        let state_root = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/wrix/workspaces")
                .join(hash.as_str())
        } else {
            env::var_os("XDG_STATE_HOME")
                .map_or_else(|| home.join(".local/state"), PathBuf::from)
                .join("wrix/workspaces")
                .join(hash.as_str())
        };
        let cache_root = if cfg!(target_os = "macos") {
            home.join("Library/Caches/wrix/workspaces")
                .join(hash.as_str())
                .join("binary-cache")
        } else {
            env::var_os("XDG_CACHE_HOME")
                .map_or_else(|| home.join(".cache"), PathBuf::from)
                .join("wrix/workspaces")
                .join(hash.as_str())
                .join("binary-cache")
        };
        Ok(Self {
            state_root,
            cache_root,
        })
    }

    pub fn state_root(&self) -> &Path {
        self.state_root.as_path()
    }

    fn state_base(&self) -> Option<&Path> {
        self.state_root.parent()
    }

    pub fn cache_root(&self) -> &Path {
        self.cache_root.as_path()
    }

    pub fn cache_lock_path(&self) -> PathBuf {
        self.state_root.join("cache.lock")
    }

    pub fn cache_status_path(&self) -> PathBuf {
        self.state_root.join("cache-status.json")
    }

    pub fn gcroots_dir(&self) -> PathBuf {
        self.state_root.join("gcroots")
    }

    pub fn keys_dir(&self) -> PathBuf {
        self.state_root.join("keys")
    }

    pub fn pending_dir(&self) -> PathBuf {
        self.state_root.join("pending")
    }

    pub fn cache_secret_path(&self) -> PathBuf {
        self.keys_dir().join("cache.secret")
    }

    pub fn cache_public_path(&self) -> PathBuf {
        self.keys_dir().join("cache.pub")
    }

    pub fn publish_roots_path(&self) -> PathBuf {
        self.state_root.join("publish-roots.json")
    }

    pub fn services_path(&self) -> PathBuf {
        self.state_root.join("services.json")
    }
}

impl Status {
    pub fn render(&self) -> String {
        let container_name = self.plan.container_name();
        format!(
            concat!(
                "workspace: {}\n",
                "workspace_hash: {}\n",
                "container: {}\n",
                "state_root: {}\n",
                "cache_root: {}\n",
                "cache_http_port: {}\n",
                "dolt_transport: {}\n",
                "dolt_socket: {}\n",
                "dolt_tcp_port: {}\n",
                "runtime: {:?}\n"
            ),
            self.plan.workspace().canonical_path().display(),
            self.plan.workspace().hash(),
            container_name,
            self.plan.paths().state_root().display(),
            self.plan.paths().cache_root().display(),
            option_port(self.plan.cache_port()),
            option_transport(self.plan.dolt()),
            option_socket(self.plan.dolt()),
            option_port(self.plan.dolt_port()),
            self.runtime
        )
    }
}

pub fn start(cache_mode: CacheMode) -> Result<Status> {
    let mut plan = Plan::for_current_dir(cache_mode)?;
    let runtime = Runtime::from_env()?;
    if plan.has_services() && runtime.reconcile_legacy_containers(&plan)? {
        plan = Plan::for_workspace(plan.workspace().clone(), cache_mode)?;
    }
    runtime.refresh_unavailable_ports(&mut plan)?;
    plan.ensure_layout()?;
    if plan.has_services() {
        runtime.ensure_running(&plan)?;
    }
    status_for_plan(plan)
}

pub fn stop(cache_mode: CacheMode) -> Result<Status> {
    let plan = Plan::for_current_dir(cache_mode)?;
    let runtime = Runtime::from_env()?;
    runtime.remove(&plan.container_name())?;
    status_for_plan(plan)
}

pub fn status(cache_mode: CacheMode) -> Result<Status> {
    let plan = Plan::for_current_dir(cache_mode)?;
    status_for_plan(plan)
}

pub fn logs(cache_mode: CacheMode) -> Result<Vec<u8>> {
    let plan = Plan::for_current_dir(cache_mode)?;
    Runtime::from_env()?.logs(&plan.container_name())
}

pub fn endpoints(cache_mode: CacheMode) -> Result<String> {
    let plan = Plan::for_current_dir(cache_mode)?;
    Ok(plan.services_json())
}

pub fn wait_for_dolt(cache_mode: CacheMode) -> Result<()> {
    let plan = Plan::for_current_dir(cache_mode)?;
    wait_for_dolt_plan(&plan)
}

fn status_for_plan(plan: Plan) -> Result<Status> {
    let runtime = Runtime::from_env()?.status(&plan.container_name())?;
    Ok(Status { runtime, plan })
}

#[derive(Clone, Copy, Debug, Default)]
struct PortLease {
    cache_http_port: Option<u16>,
    dolt_tcp_port: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct PersistedServices {
    #[serde(deserialize_with = "deserialize_workspace_hash")]
    workspace_hash: PersistedWorkspaceHash,
    #[serde(deserialize_with = "deserialize_container_name")]
    container_name: ContainerName,
    endpoints: PersistedEndpoints,
}

#[derive(Debug, Eq, PartialEq)]
enum PersistedWorkspaceHash {
    Current(WorkspaceHash),
    Legacy(LegacyWorkspaceHash),
}

#[derive(Debug, Eq, PartialEq)]
struct LegacyWorkspaceHash(String);

#[derive(Debug, Deserialize)]
struct PersistedEndpoints {
    cache_http: Option<PersistedEndpoint>,
    dolt_tcp: Option<PersistedEndpoint>,
}

#[derive(Debug, Deserialize)]
struct PersistedEndpoint {
    host: std::net::Ipv4Addr,
    port: std::num::NonZeroU16,
}

impl PersistedEndpoint {
    fn loopback_port(&self) -> Option<u16> {
        self.host.is_loopback().then_some(self.port.get())
    }
}

impl PortLease {
    fn read(path: &Path) -> Result<Self> {
        let Some(services) = read_persisted_services(path)? else {
            return Ok(Self::default());
        };
        Ok(Self {
            cache_http_port: services
                .endpoints
                .cache_http
                .as_ref()
                .and_then(PersistedEndpoint::loopback_port),
            dolt_tcp_port: services
                .endpoints
                .dolt_tcp
                .as_ref()
                .and_then(PersistedEndpoint::loopback_port),
        })
    }
}

impl PersistedWorkspaceHash {
    fn parse(value: String) -> Result<Self> {
        match WorkspaceHash::parse(&value) {
            Ok(hash) => Ok(Self::Current(hash)),
            Err(_source) if LegacyWorkspaceHash::is_valid(&value) => {
                Ok(Self::Legacy(LegacyWorkspaceHash(value)))
            }
            Err(_source) => Err(Error::InvalidPersistedWorkspaceHash { value }),
        }
    }

    fn matches(&self, current: &WorkspaceHash) -> bool {
        match self {
            Self::Current(persisted) => persisted == current,
            Self::Legacy(_legacy) => false,
        }
    }
}

impl LegacyWorkspaceHash {
    const HEX_LEN: usize = 16;

    fn is_valid(value: &str) -> bool {
        value.len() == Self::HEX_LEN
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }
}

fn deserialize_workspace_hash<'de, D>(
    deserializer: D,
) -> std::result::Result<PersistedWorkspaceHash, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    PersistedWorkspaceHash::parse(value).map_err(de::Error::custom)
}

fn deserialize_container_name<'de, D>(
    deserializer: D,
) -> std::result::Result<ContainerName, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    ContainerName::from_persisted(value)
        .ok_or_else(|| de::Error::custom("invalid service container name"))
}

fn read_persisted_services(path: &Path) -> Result<Option<PersistedServices>> {
    if !path.is_file() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content)
        .map(Some)
        .map_err(|source| Error::PersistedServicesJson {
            path: path.display().to_string(),
            source,
        })
}

fn select_container_name(workspace: &Workspace, paths: &Paths) -> Result<ContainerName> {
    if let Some(existing) = current_container_name(paths, workspace.hash())?
        && !container_name_collides(paths, &existing, workspace.hash())?
    {
        return Ok(existing);
    }
    let default = workspace.container_name();
    if container_name_collides(paths, &default, workspace.hash())? {
        Ok(workspace.disambiguated_container_name())
    } else {
        Ok(default)
    }
}

fn current_container_name(
    paths: &Paths,
    workspace_hash: &WorkspaceHash,
) -> Result<Option<ContainerName>> {
    let path = paths.services_path();
    let Some(services) = read_persisted_services(&path)? else {
        return Ok(None);
    };
    if !services.workspace_hash.matches(workspace_hash) {
        return Ok(None);
    }
    Ok(Some(services.container_name))
}

fn container_name_collides(
    paths: &Paths,
    candidate: &ContainerName,
    workspace_hash: &WorkspaceHash,
) -> Result<bool> {
    let Some(state_base) = paths.state_base() else {
        return Ok(false);
    };
    if !state_base.exists() {
        return Ok(false);
    }
    for entry in fs::read_dir(state_base)? {
        let services_path = entry?.path().join("services.json");
        if !services_path.exists() {
            continue;
        }
        let Some(services) = read_persisted_services(&services_path)? else {
            continue;
        };
        if &services.container_name == candidate && !services.workspace_hash.matches(workspace_hash)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeKind {
    Podman,
    Container,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ImageSource {
    path: PathBuf,
    kind: SourceKind,
    digest: Option<Digest>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContainerInfo {
    name: String,
    kind: Option<String>,
    workspace_hash: Option<String>,
    workspace_path: Option<String>,
    published_ports: Vec<u16>,
}

struct Runtime {
    binary: String,
    kind: RuntimeKind,
    image: String,
    image_source: Option<ImageSource>,
}

impl RuntimeKind {
    fn for_binary(binary: &str) -> Self {
        let name = Path::new(binary)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(binary);
        if name == "container" {
            Self::Container
        } else {
            Self::Podman
        }
    }
}

fn parse_image_source_kind(input: &str) -> Result<SourceKind> {
    match input {
        "nix-descriptor" => Ok(SourceKind::NixDescriptor),
        "docker-archive" => Ok(SourceKind::DockerArchive),
        other => Err(Error::UnknownImageSourceKind {
            value: other.to_owned(),
        }),
    }
}

impl ImageSource {
    fn from_env(path: PathBuf) -> Result<Self> {
        let kind = match env::var("WRIX_SERVICE_IMAGE_SOURCE_KIND") {
            Ok(value) => parse_image_source_kind(&value)?,
            Err(env::VarError::NotPresent) => {
                return Err(Error::MissingImageSourceKind);
            }
            Err(env::VarError::NotUnicode(_)) => {
                return Err(Error::InvalidUnicodeEnvironment {
                    name: "WRIX_SERVICE_IMAGE_SOURCE_KIND",
                });
            }
        };
        let digest = match env::var("WRIX_SERVICE_IMAGE_DIGEST") {
            Ok(value) if value.is_empty() => None,
            Ok(value) if value.starts_with("sha256:") => {
                Some(Digest::parse(&value).map_err(|_source| Error::InvalidImageDigest { value })?)
            }
            Ok(value) => {
                let path = Path::new(&value);
                if !path.is_file() {
                    return Err(Error::MissingImageDigestPath { path: value });
                }
                let digest = fs::read_to_string(path)?.trim().to_owned();
                Some(
                    Digest::parse(&digest)
                        .map_err(|_source| Error::InvalidImageDigest { value: digest })?,
                )
            }
            Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(Error::InvalidUnicodeEnvironment {
                    name: "WRIX_SERVICE_IMAGE_DIGEST",
                });
            }
        };
        Ok(Self { path, kind, digest })
    }
}

impl ContainerInfo {
    fn belongs_to_workspace(&self, plan: &Plan) -> bool {
        let workspace_path = plan.workspace().canonical_path().display().to_string();
        self.workspace_hash.as_deref() == Some(plan.workspace().hash().as_str())
            || self.workspace_path.as_deref() == Some(workspace_path.as_str())
    }

    fn publishes_any(&self, ports: &[u16]) -> bool {
        self.published_ports
            .iter()
            .any(|published| ports.contains(published))
    }

    fn is_legacy_service_on_selected_port(&self, ports: &[u16]) -> bool {
        self.kind.as_deref() == Some("service")
            && self.workspace_hash.is_none()
            && self.workspace_path.is_none()
            && self.publishes_any(ports)
    }

    fn owner_description(&self) -> String {
        let mut description = format!("container {}", self.name);
        if self.kind.as_deref() == Some("service") {
            description.push_str(" (wrix service");
            if let Some(hash) = &self.workspace_hash {
                description.push_str(", workspace hash ");
                description.push_str(hash);
            }
            description.push(')');
        }
        description
    }
}

impl Runtime {
    fn from_env() -> Result<Self> {
        let binary = env::var("WRIX_CONTAINER_RUNTIME").unwrap_or_else(|_| default_runtime());
        let image_source = env::var_os("WRIX_SERVICE_IMAGE_SOURCE")
            .map(PathBuf::from)
            .map(ImageSource::from_env)
            .transpose()?;
        Ok(Self {
            kind: RuntimeKind::for_binary(&binary),
            binary,
            image: env::var("WRIX_SERVICE_IMAGE")
                .unwrap_or_else(|_| String::from("localhost/wrix-service:latest")),
            image_source,
        })
    }

    fn ensure_running(&self, plan: &Plan) -> Result<()> {
        let name = plan.container_name();
        match self.status(&name)? {
            RuntimeStatus::Running => {
                self.reconcile_legacy_containers(plan)?;
                if self.running_container_satisfies_plan(&name, plan)? {
                    return Ok(());
                }
                self.remove(&name)?;
            }
            RuntimeStatus::Stopped => self.remove(&name)?,
            RuntimeStatus::Missing => {}
        }
        self.reconcile_legacy_containers(plan)?;
        self.ensure_plan_ports_available(plan)?;
        self.ensure_image()?;
        let mut command = Command::new(&self.binary);
        command
            .arg("run")
            .arg("-d")
            .arg("--name")
            .arg(name.as_str());
        if self.kind == RuntimeKind::Podman {
            command.arg("--restart=unless-stopped");
        }
        command
            .arg("--label")
            .arg(format!(
                "wrix.workspace={}",
                plan.workspace().canonical_path().display()
            ))
            .arg("--label")
            .arg(format!("wrix.workspace.hash={}", plan.workspace().hash()))
            .arg("--label")
            .arg("wrix.kind=service")
            .arg("--label")
            .arg(format!(
                "{CACHE_ENABLED_LABEL}={}",
                if plan.cache_enabled() {
                    "true"
                } else {
                    "false"
                }
            ))
            .arg("--label")
            .arg(format!(
                "{DOLT_TRANSPORT_LABEL}={}",
                expected_dolt_transport_label(plan)
            ));
        if let Some(port) = plan.cache_port() {
            command
                .arg("-p")
                .arg(format!("127.0.0.1:{port}:8080"))
                .arg("-v")
                .arg(format!("{}:/cache:ro", plan.paths().cache_root().display()));
        }
        if let Some(dolt) = plan.dolt() {
            command.arg("-v").arg(format!(
                "{}:/var/lib/wrix/beads/dolt:rw",
                plan.workspace()
                    .canonical_path()
                    .join(".beads/dolt")
                    .display()
            ));
            if let Some(remote) = plan.beads_worktree_remote() {
                command
                    .arg("-v")
                    .arg(format!("{}:{}:rw", remote.display(), remote.display()))
                    .arg("-v")
                    .arg(format!(
                        "{}:/workspace/.git/beads-worktrees/beads/.beads/dolt-remote:rw",
                        remote.display()
                    ));
            }
            match dolt.transport() {
                DoltTransport::UnixSocket => {
                    remove_stale_socket(dolt.socket_path())?;
                    if self.kind == RuntimeKind::Container {
                        command.arg("--publish-socket").arg(format!(
                            "{}:/run/wrix/dolt.sock",
                            dolt.socket_path().display()
                        ));
                    } else {
                        command.arg("-v").arg(format!(
                            "{}:/run/wrix:rw",
                            plan.workspace().canonical_path().join(".wrix").display()
                        ));
                    }
                }
                DoltTransport::Tcp => {
                    if let Some(port) = dolt.tcp_port() {
                        command.arg("-p").arg(format!("127.0.0.1:{port}:3306"));
                    }
                }
            }
        }
        command
            .arg(&self.image)
            .arg("sh")
            .arg("-c")
            .arg(container_command(plan))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let output = command.output()?;
        if output.status.success() {
            Ok(())
        } else {
            let message = String::from_utf8_lossy(&output.stderr).into_owned();
            bind_port_from_runtime_error(&message).map_or_else(
                || Err(Error::Operation { message }),
                |port| Err(self.port_in_use_error(port)),
            )
        }
    }

    fn running_container_satisfies_plan(&self, name: &ContainerName, plan: &Plan) -> Result<bool> {
        if plan.cache_enabled()
            && self
                .inspect_label(name.as_str(), CACHE_ENABLED_LABEL)?
                .as_deref()
                != Some("true")
        {
            return Ok(false);
        }
        let expected_dolt = expected_dolt_transport_label(plan);
        let actual_dolt = self.inspect_label(name.as_str(), DOLT_TRANSPORT_LABEL)?;
        if actual_dolt.as_deref() != Some(expected_dolt)
            && !(actual_dolt.is_none() && expected_dolt == DOLT_DISABLED_LABEL_VALUE)
        {
            return Ok(false);
        }
        if let Some(dolt) = plan.dolt()
            && dolt.transport() == DoltTransport::UnixSocket
            && !is_unix_socket(dolt.socket_path())?
        {
            return Ok(false);
        }
        if self.kind == RuntimeKind::Podman {
            let published_ports = self.published_ports(name.as_str())?;
            return Ok(plan
                .selected_host_ports()
                .iter()
                .all(|port| published_ports.contains(port)));
        }
        Ok(true)
    }

    fn reconcile_legacy_containers(&self, plan: &Plan) -> Result<bool> {
        let planned_name = plan.container_name();
        let selected_ports = plan.selected_host_ports();
        let mut removed = false;
        for container in self.list_container_infos()? {
            if container.name == planned_name.as_str() {
                continue;
            }
            if container.belongs_to_workspace(plan)
                || container.is_legacy_service_on_selected_port(&selected_ports)
            {
                self.remove_identifier(&container.name)?;
                removed = true;
            }
        }
        Ok(removed)
    }

    fn refresh_unavailable_ports(&self, plan: &mut Plan) -> Result<()> {
        if let Some(port) = plan.cache_port
            && !self.port_available_for_plan(port, plan)?
        {
            plan.cache_port =
                Some(self.select_available_port(plan, CACHE_PORT_START, CACHE_PORT_WIDTH)?);
        }
        let dolt_port = plan.dolt.as_ref().and_then(|endpoint| endpoint.tcp_port);
        let replace_dolt_port = match dolt_port {
            Some(port) => !self.port_available_for_plan(port, plan)?,
            None => false,
        };
        if replace_dolt_port {
            let port = self.select_available_port(plan, DOLT_PORT_START, DOLT_PORT_WIDTH)?;
            if let Some(endpoint) = plan.dolt.as_mut() {
                endpoint.tcp_port = Some(port);
            }
        }
        Ok(())
    }

    fn select_available_port(&self, plan: &Plan, start: u16, width: u16) -> Result<u16> {
        let preferred = start + plan.workspace.hash().port_offset(width);
        for offset in 0..width {
            let port = start + ((preferred - start + offset) % width);
            if self.port_available_for_plan(port, plan)? {
                return Ok(port);
            }
        }
        Err(Error::NoAvailableLoopbackPort {
            start,
            end: start + width - 1,
        })
    }

    fn port_available_for_plan(&self, port: u16, plan: &Plan) -> Result<bool> {
        if let Some(owner) = self.container_port_owner(port)? {
            return Ok(
                owner.name == plan.container_name().as_str() && owner.belongs_to_workspace(plan)
            );
        }
        Ok(is_loopback_port_available(port))
    }

    fn ensure_plan_ports_available(&self, plan: &Plan) -> Result<()> {
        for port in plan.selected_host_ports() {
            if let Some(owner) = self.container_port_owner(port)? {
                return Err(Error::PortInUse {
                    port,
                    owner: owner.owner_description(),
                });
            }
            if !is_loopback_port_available(port) {
                return Err(self.port_in_use_error(port));
            }
        }
        Ok(())
    }

    fn port_in_use_error(&self, port: u16) -> Error {
        let owner = match self.describe_port_owner(port) {
            Ok(value) => value,
            Err(error) => format!("an unknown owner; owner lookup failed: {error}"),
        };
        Error::PortInUse { port, owner }
    }

    fn describe_port_owner(&self, port: u16) -> Result<String> {
        if let Some(owner) = self.container_port_owner(port)? {
            return Ok(owner.owner_description());
        }
        Ok(process_port_owner(port)
            .unwrap_or_else(|| format!("an unknown process on 127.0.0.1:{port}")))
    }

    fn container_port_owner(&self, port: u16) -> Result<Option<ContainerInfo>> {
        for container in self.list_container_infos()? {
            if container.published_ports.contains(&port) {
                return Ok(Some(container));
            }
        }
        Ok(None)
    }

    fn list_container_infos(&self) -> Result<Vec<ContainerInfo>> {
        if self.kind == RuntimeKind::Container {
            return Ok(Vec::new());
        }
        let names = self.list_container_names()?;
        let mut containers = Vec::with_capacity(names.len());
        for name in names {
            containers.push(ContainerInfo {
                kind: self.inspect_label(&name, "wrix.kind")?,
                workspace_hash: self.inspect_label(&name, "wrix.workspace.hash")?,
                workspace_path: self.inspect_label(&name, "wrix.workspace")?,
                published_ports: self.published_ports(&name)?,
                name,
            });
        }
        Ok(containers)
    }

    fn list_container_names(&self) -> Result<Vec<String>> {
        let output = Command::new(&self.binary)
            .arg("ps")
            .arg("-a")
            .arg("--format")
            .arg("{{.Names}}")
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(Error::Operation {
                message: format!(
                    "failed to list containers: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn inspect_label(&self, name: &str, label: &str) -> Result<Option<String>> {
        let template = format!("{{{{ index .Config.Labels \"{label}\" }}}}");
        self.inspect_format(name, &template)
    }

    fn inspect_format(&self, name: &str, template: &str) -> Result<Option<String>> {
        let output = Command::new(&self.binary)
            .arg("inspect")
            .arg("--format")
            .arg(template)
            .arg(name)
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            let output_text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if is_missing_container_remove_error(&output_text) {
                return Ok(None);
            }
            return Err(Error::Operation {
                message: format!("failed to inspect container {name}: {output_text}"),
            });
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if value.is_empty() || value == "<no value>" {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    fn published_ports(&self, name: &str) -> Result<Vec<u16>> {
        let output = Command::new(&self.binary)
            .arg("port")
            .arg(name)
            .stdin(Stdio::null())
            .output()?;
        if output.status.success() {
            return Ok(parse_published_ports(&String::from_utf8_lossy(
                &output.stdout,
            )));
        }
        let output_text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if is_missing_container_remove_error(&output_text) {
            Ok(Vec::new())
        } else {
            Err(Error::Operation {
                message: format!(
                    "failed to inspect published ports for container {name}: {output_text}"
                ),
            })
        }
    }

    fn ensure_image(&self) -> Result<()> {
        let Some(source) = &self.image_source else {
            return Ok(());
        };
        let runtime = match self.kind {
            RuntimeKind::Podman => ImageRuntime::Podman,
            RuntimeKind::Container => ImageRuntime::Container,
        };
        let source_path = source.path.display().to_string();
        let mut store = CommandStore;
        runtime_image::install(
            &mut store,
            &InstallRequest {
                runtime,
                image_ref: &self.image,
                image_source: &source_path,
                source_kind: source.kind,
                digest: source.digest.as_ref(),
            },
        )?;
        let mru_path = runtime_image::default_mru_path();
        runtime_image::remember_and_prune(
            &mut store,
            &RetentionRequest {
                runtime,
                image_ref: &self.image,
                image_source: &source_path,
                source_kind: source.kind,
                digest: source.digest.as_ref(),
                mru_path: &mru_path,
            },
        )?;
        if self.image_exists()? {
            Ok(())
        } else {
            Err(Error::InstalledImageUnavailable {
                path: source_path,
                image: self.image.clone(),
            })
        }
    }

    fn image_exists(&self) -> Result<bool> {
        let mut command = Command::new(&self.binary);
        command.arg("image");
        match self.kind {
            RuntimeKind::Container => {
                command.arg("inspect");
            }
            RuntimeKind::Podman => {
                command.arg("exists");
            }
        }
        let status = command
            .arg(&self.image)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        Ok(status.success())
    }

    fn remove(&self, name: &ContainerName) -> Result<()> {
        self.remove_identifier(name.as_str())
            .map_err(|error| Error::Operation {
                message: format!("failed to remove service container {name}: {error}"),
            })
    }

    fn remove_identifier(&self, identifier: &str) -> Result<()> {
        let output = Command::new(&self.binary)
            .arg("rm")
            .arg("-f")
            .arg(identifier)
            .stdin(Stdio::null())
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            let output_text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if is_missing_container_remove_error(&output_text) {
                Ok(())
            } else {
                Err(Error::Operation {
                    message: output_text,
                })
            }
        }
    }

    fn logs(&self, name: &ContainerName) -> Result<Vec<u8>> {
        let output = Command::new(&self.binary)
            .arg("logs")
            .arg(name.as_str())
            .stdin(Stdio::null())
            .output()?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(Error::Operation {
                message: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        }
    }

    fn status(&self, name: &ContainerName) -> Result<RuntimeStatus> {
        let mut command = Command::new(&self.binary);
        match self.kind {
            RuntimeKind::Container => {
                command.arg("inspect");
            }
            RuntimeKind::Podman => {
                command
                    .arg("inspect")
                    .arg("--format")
                    .arg("{{.State.Running}}");
            }
        }
        let output = command.arg(name.as_str()).stdin(Stdio::null()).output()?;
        if !output.status.success() {
            return Ok(RuntimeStatus::Missing);
        }
        match self.kind {
            RuntimeKind::Container => {
                let text = String::from_utf8_lossy(&output.stdout);
                if text.contains(r#""status":"running""#) {
                    Ok(RuntimeStatus::Running)
                } else {
                    Ok(RuntimeStatus::Stopped)
                }
            }
            RuntimeKind::Podman => {
                if String::from_utf8_lossy(&output.stdout).trim() == "true" {
                    Ok(RuntimeStatus::Running)
                } else {
                    Ok(RuntimeStatus::Stopped)
                }
            }
        }
    }
}

fn is_missing_container_remove_error(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    text.contains("notfound")
        || text.contains("not found")
        || text.contains("no such container")
        || text.contains("no such object")
        || text.contains("no container with")
}

fn parse_published_ports(input: &str) -> Vec<u16> {
    let mut ports = Vec::new();
    for line in input.lines() {
        for token in line.split_whitespace() {
            let token = token.trim_matches(|ch| matches!(ch, ',' | ';'));
            let Some((_host, port_text)) = token.rsplit_once(':') else {
                continue;
            };
            let digits = port_text
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            if let Ok(port) = digits.parse::<u16>()
                && !ports.contains(&port)
            {
                ports.push(port);
            }
        }
    }
    ports
}

fn bind_port_from_runtime_error(message: &str) -> Option<u16> {
    let lower = message.to_ascii_lowercase();
    let marker = "failed to bind port ";
    let start = lower.find(marker)? + marker.len();
    let digits = message[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    let Ok(port) = digits.parse::<u16>() else {
        return None;
    };
    Some(port)
}

fn process_port_owner(port: u16) -> Option<String> {
    lsof_port_owner(port).or_else(|| ss_port_owner(port))
}

fn lsof_port_owner(port: u16) -> Option<String> {
    let Ok(output) = Command::new("lsof")
        .arg("-nP")
        .arg(format!("-iTCP@127.0.0.1:{port}"))
        .arg("-sTCP:LISTEN")
        .stdin(Stdio::null())
        .output()
    else {
        return None;
    };
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| format!("process reported by lsof ({line})"))
}

fn ss_port_owner(port: u16) -> Option<String> {
    let Ok(output) = Command::new("ss")
        .arg("-H")
        .arg("-ltnp")
        .stdin(Stdio::null())
        .output()
    else {
        return None;
    };
    if !output.status.success() {
        return None;
    }
    let needle = format!(":{port}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| line.contains(&needle))
        .map(|line| format!("process reported by ss ({line})"))
}

fn default_runtime() -> String {
    if cfg!(target_os = "macos") {
        String::from("container")
    } else {
        String::from("podman")
    }
}

fn cache_allowed_for_workspace(path: &Path) -> bool {
    env::var("WRIX_SERVICE_ALLOW_TEMP_CACHE").is_ok_and(|value| value == "1")
        || !temp_roots().iter().any(|root| path.starts_with(root))
}

fn temp_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/var/tmp"),
    ];
    for root in &mut roots {
        if let Ok(canonical) = root.canonicalize() {
            *root = canonical;
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn select_port(prior: Option<u16>, start: u16, width: u16, hash: &WorkspaceHash) -> Result<u16> {
    if let Some(port) = prior
        && (start..start + width).contains(&port)
    {
        return Ok(port);
    }
    let preferred = start + hash.port_offset(width);
    for offset in 0..width {
        let port = start + ((preferred - start + offset) % width);
        if is_loopback_port_available(port) {
            return Ok(port);
        }
    }
    Err(Error::NoAvailableLoopbackPort {
        start,
        end: start + width - 1,
    })
}

fn is_loopback_port_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn container_command(plan: &Plan) -> String {
    match (plan.cache_enabled(), plan.dolt()) {
        (true, Some(dolt)) => format!("wrix-cache-serve /cache & {}", dolt_server_command(dolt)),
        (false, Some(dolt)) => dolt_server_command(dolt),
        (true, None) => String::from("exec wrix-cache-serve /cache"),
        (false, None) => String::from("sleep infinity"),
    }
}

fn dolt_server_command(dolt: &DoltEndpoint) -> String {
    let base = "mkdir -p /run/wrix && exec dolt sql-server --data-dir /var/lib/wrix/beads/dolt";
    match dolt.transport() {
        DoltTransport::UnixSocket => {
            format!("{base} --host 127.0.0.1 --socket /run/wrix/dolt.sock")
        }
        DoltTransport::Tcp => format!("{base} --host 0.0.0.0 --port 3306"),
    }
}

fn expected_dolt_transport_label(plan: &Plan) -> &'static str {
    plan.dolt()
        .map_or(DOLT_DISABLED_LABEL_VALUE, |dolt| dolt.transport().as_str())
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::StaleDoltSocketRemoval {
            path: path.display().to_string(),
            source,
        }),
    }
}

#[cfg(unix)]
fn is_unix_socket(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_socket()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(source.into()),
    }
}

#[cfg(not(unix))]
fn is_unix_socket(_path: &Path) -> Result<bool> {
    Ok(false)
}

fn wait_for_dolt_plan(plan: &Plan) -> Result<()> {
    let Some(dolt) = plan.dolt() else {
        return Ok(());
    };
    let deadline = Instant::now() + DOLT_READY_TIMEOUT;
    loop {
        match probe_dolt_endpoint(dolt) {
            Ok(()) => return Ok(()),
            Err(source) if Instant::now() >= deadline => {
                return Err(Error::DoltEndpointUnavailable {
                    endpoint: dolt_endpoint_description(dolt),
                    timeout: DOLT_READY_TIMEOUT.as_secs(),
                    source,
                });
            }
            Err(_) => {}
        }
        thread::sleep(DOLT_READY_INTERVAL);
    }
}

fn probe_dolt_endpoint(dolt: &DoltEndpoint) -> io::Result<()> {
    match dolt.transport() {
        DoltTransport::UnixSocket => connect_unix_socket(dolt.socket_path()),
        DoltTransport::Tcp => {
            let Some(port) = dolt.tcp_port() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Dolt TCP endpoint has no port",
                ));
            };
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            TcpStream::connect_timeout(&addr, DOLT_READY_INTERVAL).map(|_| ())
        }
    }
}

#[cfg(unix)]
fn connect_unix_socket(path: &Path) -> io::Result<()> {
    UnixStream::connect(path).map(|_| ())
}

#[cfg(not(unix))]
fn connect_unix_socket(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix sockets are not supported on this platform",
    ))
}

fn dolt_endpoint_description(dolt: &DoltEndpoint) -> String {
    match dolt.transport() {
        DoltTransport::UnixSocket => dolt.socket_path().display().to_string(),
        DoltTransport::Tcp => format!("127.0.0.1:{}", option_port_value(dolt.tcp_port())),
    }
}

fn json_port(port: Option<u16>) -> String {
    port.map_or_else(
        || String::from("null"),
        |value| format!("{{ \"host\": \"127.0.0.1\", \"port\": {value} }}"),
    )
}

fn json_dolt_endpoint(dolt: Option<&DoltEndpoint>) -> String {
    dolt.map_or_else(
        || String::from("null"),
        |endpoint| match endpoint.transport() {
            DoltTransport::UnixSocket => format!(
                "{{ \"transport\": \"unix\", \"socket\": \"{}\", \"env\": {{ \"BEADS_DOLT_SERVER_SOCKET\": \"{}\" }} }}",
                escape_json(&endpoint.socket_path().display().to_string()),
                escape_json(&endpoint.socket_path().display().to_string())
            ),
            DoltTransport::Tcp => format!(
                "{{ \"transport\": \"tcp\", \"host\": \"127.0.0.1\", \"port\": {}, \"env\": {{ \"BEADS_DOLT_SERVER_HOST\": \"127.0.0.1\", \"BEADS_DOLT_SERVER_PORT\": \"{}\" }} }}",
                option_port_value(endpoint.tcp_port()),
                option_port_value(endpoint.tcp_port())
            ),
        },
    )
}

fn json_dolt_unix(dolt: Option<&DoltEndpoint>) -> String {
    match dolt {
        Some(endpoint) if endpoint.transport() == DoltTransport::UnixSocket => format!(
            "{{ \"socket\": \"{}\" }}",
            escape_json(&endpoint.socket_path().display().to_string())
        ),
        _ => String::from("null"),
    }
}

fn json_dolt_tcp(dolt: Option<&DoltEndpoint>) -> String {
    match dolt {
        Some(endpoint) if endpoint.transport() == DoltTransport::Tcp => {
            json_port(endpoint.tcp_port())
        }
        _ => String::from("null"),
    }
}

fn option_port(port: Option<u16>) -> String {
    port.map_or_else(|| String::from("disabled"), |value| value.to_string())
}

fn option_port_value(port: Option<u16>) -> String {
    port.map_or_else(|| String::from("null"), |value| value.to_string())
}

fn option_transport(dolt: Option<&DoltEndpoint>) -> String {
    dolt.map_or_else(
        || String::from("disabled"),
        |endpoint| endpoint.transport().as_str().to_owned(),
    )
}

fn option_socket(dolt: Option<&DoltEndpoint>) -> String {
    dolt.map_or_else(
        || String::from("disabled"),
        |endpoint| endpoint.socket_path().display().to_string(),
    )
}

fn default_cache_status() -> String {
    String::from(
        "{\n  \"dirty\": false,\n  \"last_publish\": null,\n  \"last_prune\": null,\n  \"last_error\": null\n}\n",
    )
}

fn nix_cache_info() -> String {
    String::from("StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 40\n")
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::Operation {
            message: String::from("HOME is required to resolve wrix service state roots"),
        })
}

fn write_if_missing(path: &Path, content: impl AsRef<[u8]>) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, content)?;
    Ok(())
}

fn escape_json(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            value if value.is_control() => push_unicode_escape(&mut escaped, value),
            value => escaped.push(value),
        }
    }
    escaped
}

fn push_unicode_escape(output: &mut String, value: char) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let codepoint = u32::from(value);
    output.push_str("\\u");
    for shift in [12, 8, 4, 0] {
        let nibble = ((codepoint >> shift) & 0x0f) as usize;
        output.push(char::from(HEX[nibble]));
    }
}

#[cfg(test)]
mod test {
    use super::{
        DoltTransport, Plan, RuntimeKind, bind_port_from_runtime_error,
        is_missing_container_remove_error, parse_image_source_kind, parse_published_ports,
    };
    use wrix_core::path::Workspace;
    use wrix_sandbox::image::SourceKind;

    #[test]
    fn published_port_parser_extracts_loopback_bindings() {
        let ports =
            parse_published_ports("8080/tcp -> 127.0.0.1:21232\n3306/tcp -> 127.0.0.1:23042\n");

        assert_eq!(ports, vec![21_232, 23_042]);
    }

    #[test]
    fn runtime_bind_error_parser_extracts_pasta_port() {
        let message =
            "pasta failed with exit code 1:\nFailed to bind port 21232 (Address already in use)";

        assert_eq!(bind_port_from_runtime_error(message), Some(21_232));
    }

    #[test]
    fn missing_container_parser_accepts_podman_no_such_object() {
        assert!(is_missing_container_remove_error(
            "Error: no such object: \"admiring_albattani\""
        ));
    }

    #[test]
    fn service_image_source_kind_parser_accepts_contract_values() {
        assert_eq!(
            parse_image_source_kind("nix-descriptor").unwrap(),
            SourceKind::NixDescriptor
        );
        assert_eq!(
            parse_image_source_kind("docker-archive").unwrap(),
            SourceKind::DockerArchive
        );
        assert!(parse_image_source_kind("tarball").is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn platform_default_dolt_transport_is_tcp_on_macos() {
        assert_eq!(DoltTransport::platform_default(), DoltTransport::Tcp);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn platform_default_dolt_transport_is_unix_off_macos() {
        assert_eq!(DoltTransport::platform_default(), DoltTransport::UnixSocket);
    }

    #[test]
    fn runtime_kind_parser_accepts_container_path() {
        assert_eq!(RuntimeKind::for_binary("container"), RuntimeKind::Container);
        assert_eq!(
            RuntimeKind::for_binary("/nix/store/example/bin/container"),
            RuntimeKind::Container
        );
        assert_eq!(RuntimeKind::for_binary("podman"), RuntimeKind::Podman);
    }

    #[test]
    fn plan_uses_workspace_identity_for_ports() {
        let workspace = Workspace::from_current_dir().unwrap();
        let plan = Plan::for_workspace(workspace.clone(), super::CacheMode::Enabled).unwrap();
        let second = Plan::for_workspace(workspace, super::CacheMode::Enabled).unwrap();
        assert_eq!(plan.cache_port(), second.cache_port());
        assert_eq!(plan.paths().state_root(), second.paths().state_root());
    }
}
