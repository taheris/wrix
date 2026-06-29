use std::{
    env, fs, io,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use displaydoc::Display;
use thiserror::Error as ThisError;
use wrix_core::{
    cache_key,
    path::{ContainerName, Workspace, WorkspaceHash},
};

const SCHEMA_VERSION: u8 = 1;
const CACHE_PORT_START: u16 = 21_000;
const CACHE_PORT_WIDTH: u16 = 2_000;
const DOLT_PORT_START: u16 = 23_000;
const DOLT_PORT_WIDTH: u16 = 2_000;
const CACHE_ENABLED_LABEL: &str = "wrix.cache.enabled";

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// service lifecycle I/O failed
    Io {
        #[from]
        source: io::Error,
    },
    /// environment variable {name} must be valid Unicode
    InvalidUnicodeEnvironment { name: &'static str },
    /// unknown Dolt transport: {value}
    UnknownDoltTransport { value: String },
    /// unknown service image source kind: {value}
    UnknownImageSourceKind { value: String },
    /// `WRIX_SERVICE_IMAGE_SOURCE_KIND` is required when `WRIX_SERVICE_IMAGE_SOURCE` is set
    MissingImageSourceKind,
    /// service image digest path does not exist: {path}
    MissingImageDigestPath { path: String },
    /// service image descriptor {path} is missing field {field}
    MissingImageDescriptorField { path: String, field: &'static str },
    /// service image descriptor {path} has invalid field {field}: {value}
    InvalidImageDescriptorField {
        path: String,
        field: &'static str,
        value: String,
    },
    /// container runtime requires service image source_kind=docker-archive
    ContainerRequiresDockerArchive,
    /// installed service image from {path}, but image {image} is unavailable
    InstalledImageUnavailable { path: String, image: String },
    /// service lifecycle operation failed: {message}
    Operation { message: String },
    /// service port {port} is already in use by {owner}; stop that process/container or free the port before retrying
    PortInUse { port: u16, owner: String },
    /// no available loopback port in {start}-{end}
    NoAvailableLoopbackPort { start: u16, end: u16 },
    /// could not create a unique {prefix} temp directory
    TempDirAttemptsExceeded { prefix: String },
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

    fn ensure_layout(&self) -> Result<()> {
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
        Self::UnixSocket
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnixSocket => "unix",
            Self::Tcp => "tcp",
        }
    }
}

impl Paths {
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
    let path = plan.paths().services_path();
    if path.exists() {
        Ok(fs::read_to_string(path)?)
    } else {
        Ok(plan.services_json())
    }
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

impl PortLease {
    fn read(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path)?;
        Ok(Self {
            cache_http_port: read_endpoint_port(&content, "cache_http"),
            dolt_tcp_port: read_endpoint_port(&content, "dolt_tcp"),
        })
    }
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
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    if json_string_field(&content, "workspace_hash").as_deref() != Some(workspace_hash.as_str()) {
        return Ok(None);
    }
    Ok(json_string_field(&content, "container_name").and_then(ContainerName::from_persisted))
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
        let content = fs::read_to_string(services_path)?;
        let Some(container_name) = json_string_field(&content, "container_name") else {
            continue;
        };
        let Some(other_hash) = json_string_field(&content, "workspace_hash") else {
            continue;
        };
        if container_name == candidate.as_str() && other_hash != workspace_hash.as_str() {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageSourceKind {
    NixDescriptor,
    DockerArchive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ImageSource {
    path: PathBuf,
    kind: ImageSourceKind,
    digest: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DescriptorSource {
    digest: String,
    oci_layout: PathBuf,
    oci_ref: String,
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

impl ImageSourceKind {
    fn parse(input: &str) -> Result<Self> {
        match input {
            "nix-descriptor" => Ok(Self::NixDescriptor),
            "docker-archive" => Ok(Self::DockerArchive),
            other => Err(Error::UnknownImageSourceKind {
                value: other.to_owned(),
            }),
        }
    }
}

impl ImageSource {
    fn from_env(path: PathBuf) -> Result<Self> {
        let kind = match env::var("WRIX_SERVICE_IMAGE_SOURCE_KIND") {
            Ok(value) => ImageSourceKind::parse(&value)?,
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
            Ok(value) => Some(value),
            Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(Error::InvalidUnicodeEnvironment {
                    name: "WRIX_SERVICE_IMAGE_DIGEST",
                });
            }
        };
        Ok(Self { path, kind, digest })
    }

    fn desired_digest(&self) -> Result<Option<String>> {
        let Some(value) = &self.digest else {
            return self.descriptor_digest();
        };
        if value.starts_with("sha256:") {
            return Ok(Some(value.clone()));
        }
        let path = Path::new(value);
        if !path.exists() {
            return Err(Error::MissingImageDigestPath {
                path: path.display().to_string(),
            });
        }
        let digest = fs::read_to_string(path)?.trim().to_owned();
        if digest.is_empty() {
            Ok(None)
        } else {
            Ok(Some(digest))
        }
    }

    fn descriptor_digest(&self) -> Result<Option<String>> {
        if self.kind == ImageSourceKind::NixDescriptor {
            Ok(Some(DescriptorSource::from_path(&self.path)?.digest))
        } else {
            Ok(None)
        }
    }
}

impl DescriptorSource {
    fn from_path(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        Self::from_json(path, &content)
    }

    fn from_json(path: &Path, content: &str) -> Result<Self> {
        let digest = required_descriptor_field(path, content, "digest")?;
        if !is_sha256_digest(&digest) {
            return Err(Error::InvalidImageDescriptorField {
                path: path.display().to_string(),
                field: "digest",
                value: digest,
            });
        }
        Ok(Self {
            digest,
            oci_layout: PathBuf::from(required_descriptor_field(path, content, "oci_layout")?),
            oci_ref: required_descriptor_field(path, content, "oci_ref")?,
        })
    }

    fn skopeo_source(&self) -> String {
        format!("oci:{}:{}", self.oci_layout.display(), self.oci_ref)
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
                    .arg(format!("{}:{}:rw", remote.display(), remote.display()));
            }
            match dolt.transport() {
                DoltTransport::UnixSocket => {
                    if self.kind == RuntimeKind::Container {
                        let _ = fs::remove_file(dolt.socket_path());
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
        if self.image_cached(source)? {
            return Ok(());
        }
        self.install_image(source)?;
        if self.image_exists()? || self.tag_loaded_image()? {
            Ok(())
        } else {
            Err(Error::InstalledImageUnavailable {
                path: source.path.display().to_string(),
                image: self.image.clone(),
            })
        }
    }

    fn image_cached(&self, source: &ImageSource) -> Result<bool> {
        if let Some(digest) = source.desired_digest()?
            && self.image_digest_exists(&digest)?
            && (self.tag_image(&digest, &self.image)? || self.image_exists()?)
        {
            return Ok(true);
        }
        self.image_exists()
    }

    fn image_digest_exists(&self, digest: &str) -> Result<bool> {
        if self.kind == RuntimeKind::Container {
            return Ok(false);
        }
        let status = Command::new(&self.binary)
            .arg("image")
            .arg("inspect")
            .arg("--format")
            .arg("{{.Id}}")
            .arg(digest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        Ok(status.success())
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

    fn install_image(&self, source: &ImageSource) -> Result<()> {
        match self.kind {
            RuntimeKind::Container => self.load_container_image(source),
            RuntimeKind::Podman => self.copy_podman_image(source),
        }
    }

    fn load_container_image(&self, source: &ImageSource) -> Result<()> {
        if source.kind != ImageSourceKind::DockerArchive {
            return Err(Error::ContainerRequiresDockerArchive);
        }
        let temp_dir = create_temp_dir("wrix-service-image")?;
        let result = self.load_container_image_in_temp(source, &temp_dir);
        match (result, fs::remove_dir_all(&temp_dir)) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) => Err(error.into()),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(cleanup_error)) => Err(Error::Operation {
                message: format!(
                    "{error}; failed to remove temporary directory {}: {cleanup_error}",
                    temp_dir.display()
                ),
            }),
        }
    }

    fn load_container_image_in_temp(&self, source: &ImageSource, temp_dir: &Path) -> Result<()> {
        let oci_archive = temp_dir.join("image.oci");
        let source_ref = format!("docker-archive:{}", source.path.display());
        let archive_ref = format!("oci-archive:{}", oci_archive.display());
        match skopeo_copy(&source_ref, &archive_ref)? {
            Ok(()) => {}
            Err(error) => return Err(Error::Operation { message: error }),
        }
        let output = Command::new(&self.binary)
            .arg("image")
            .arg("load")
            .arg("--input")
            .arg(&oci_archive)
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(Error::Operation {
                message: format!(
                    "failed to load service image from {}: {}",
                    source.path.display(),
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        if let Some(loaded_ref) = loaded_container_ref(&output.stdout, &output.stderr)
            && !self.tag_image(&loaded_ref, &self.image)?
        {
            return Err(Error::Operation {
                message: format!(
                    "failed to tag loaded service image {loaded_ref} as {}",
                    self.image
                ),
            });
        }
        Ok(())
    }

    fn copy_podman_image(&self, source: &ImageSource) -> Result<()> {
        let store_ref = self.container_storage_ref()?;
        let source_ref = match source.kind {
            ImageSourceKind::NixDescriptor => {
                DescriptorSource::from_path(&source.path)?.skopeo_source()
            }
            ImageSourceKind::DockerArchive => format!("docker-archive:{}", source.path.display()),
        };
        match skopeo_copy(&source_ref, &store_ref)? {
            Ok(()) => self.tag_latest(),
            Err(error) => Err(Error::Operation { message: error }),
        }
    }

    fn container_storage_ref(&self) -> Result<String> {
        let default = format!("containers-storage:{}", self.image);
        let output = Command::new(&self.binary)
            .arg("info")
            .arg("--format")
            .arg("{{.Store.GraphDriverName}}@{{.Store.GraphRoot}}+{{.Store.RunRoot}}")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Ok(default);
        }
        let spec = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if spec.contains('@') && spec.contains('+') {
            Ok(format!("containers-storage:[{spec}]{}", self.image))
        } else {
            Ok(default)
        }
    }

    fn tag_loaded_image(&self) -> Result<bool> {
        for source in ["wrix-service:latest", "localhost/wrix-service:latest"] {
            if source == self.image {
                continue;
            }
            if self.tag_image(source, &self.image)? && self.image_exists()? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn tag_latest(&self) -> Result<()> {
        let Some(repo) = self.image.rsplit_once(':').map(|(repo, _tag)| repo) else {
            return Ok(());
        };
        let latest = format!("{repo}:latest");
        if self.tag_image(&self.image, &latest)? {
            Ok(())
        } else {
            Err(Error::Operation {
                message: format!("failed to tag service image {} as {latest}", self.image),
            })
        }
    }

    fn tag_image(&self, source: &str, target: &str) -> Result<bool> {
        let mut command = Command::new(&self.binary);
        match self.kind {
            RuntimeKind::Container => {
                command.arg("image").arg("tag");
            }
            RuntimeKind::Podman => {
                command.arg("tag");
            }
        }
        let status = command
            .arg(source)
            .arg(target)
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

fn skopeo_copy(source_ref: &str, store_ref: &str) -> Result<std::result::Result<(), String>> {
    let output = Command::new("skopeo")
        .arg("--insecure-policy")
        .arg("copy")
        .arg("--quiet")
        .arg(source_ref)
        .arg(store_ref)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(Ok(()))
    } else {
        Ok(Err(format!(
            "skopeo copy failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )))
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
    message[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
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

fn create_temp_dir(prefix: &str) -> Result<PathBuf> {
    for attempt in 0..100 {
        let path = env::temp_dir().join(format!("{prefix}-{}-{attempt}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::TempDirAttemptsExceeded {
        prefix: prefix.to_owned(),
    })
}

fn required_descriptor_field(path: &Path, content: &str, field: &'static str) -> Result<String> {
    match json_string_field(content, field) {
        Some(value) if !value.is_empty() => Ok(value),
        Some(value) => Err(Error::InvalidImageDescriptorField {
            path: path.display().to_string(),
            field,
            value,
        }),
        None => Err(Error::MissingImageDescriptorField {
            path: path.display().to_string(),
            field,
        }),
    }
}

fn is_sha256_digest(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn json_string_field(input: &str, name: &str) -> Option<String> {
    let marker = format!("\"{name}\"");
    let start = input.find(&marker)?;
    let after_name = &input[start + marker.len()..];
    let after_colon = &after_name[after_name.find(':')? + 1..];
    let value_start = after_colon.find(|ch: char| !ch.is_whitespace())?;
    parse_json_string(&after_colon[value_start..])
}

fn parse_json_string(input: &str) -> Option<String> {
    if !input.starts_with('"') {
        return None;
    }
    let mut output = String::new();
    let mut escaped = false;
    for ch in input[1..].chars() {
        if escaped {
            match ch {
                '"' => output.push('"'),
                '\\' => output.push('\\'),
                '/' => output.push('/'),
                'b' => output.push('\u{0008}'),
                'f' => output.push('\u{000c}'),
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                _ => return None,
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(output);
        } else {
            output.push(ch);
        }
    }
    None
}

fn loaded_container_ref(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    for token in text.split_whitespace() {
        let token = token.trim_matches(|ch| matches!(ch, '"' | '\'' | ',' | ';'));
        let Some(rest) = token.strip_prefix("untagged@sha256:") else {
            continue;
        };
        let digest = rest
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect::<String>();
        if !digest.is_empty() {
            return Some(format!("untagged@sha256:{digest}"));
        }
    }
    None
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

fn read_endpoint_port(content: &str, name: &str) -> Option<u16> {
    let marker = format!("\"{name}\"");
    let start = content.find(&marker)?;
    let port_marker = "\"port\":";
    let port_start = content[start..].find(port_marker)? + start + port_marker.len();
    let digits = content[port_start..]
        .chars()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().ok()
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
    use std::path::Path;

    use super::{
        DescriptorSource, Error, ImageSourceKind, Plan, RuntimeKind, bind_port_from_runtime_error,
        is_missing_container_remove_error, loaded_container_ref, parse_published_ports,
        read_endpoint_port,
    };
    use wrix_core::path::Workspace;

    #[test]
    fn endpoint_port_reader_extracts_named_port() {
        let content = r#"{"endpoints":{"cache_http":{"host":"127.0.0.1","port":21042},"dolt_tcp":{"host":"127.0.0.1","port":23042}}}"#;
        assert_eq!(read_endpoint_port(content, "cache_http"), Some(21_042));
        assert_eq!(read_endpoint_port(content, "dolt_tcp"), Some(23_042));
    }

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
            ImageSourceKind::parse("nix-descriptor").unwrap(),
            ImageSourceKind::NixDescriptor
        );
        assert_eq!(
            ImageSourceKind::parse("docker-archive").unwrap(),
            ImageSourceKind::DockerArchive
        );
        assert!(ImageSourceKind::parse("tarball").is_err());
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
    fn service_descriptor_source_uses_oci_layout_transport() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let content = format!(
            r#"{{"schema":1,"digest":"{digest}","oci_layout":"/nix/store/fake-oci","oci_ref":"latest"}}"#
        );
        let descriptor =
            DescriptorSource::from_json(Path::new("/nix/store/fake-descriptor.json"), &content)
                .unwrap();

        assert_eq!(descriptor.digest, digest);
        assert_eq!(descriptor.skopeo_source(), "oci:/nix/store/fake-oci:latest");
    }

    #[test]
    fn service_descriptor_source_rejects_stream_fallback_without_layout() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let legacy_stream_key = "fallback_".to_owned() + "stream";
        let content = format!(
            r#"{{"schema":1,"digest":"{digest}","{legacy_stream_key}":"/nix/store/fake-stream"}}"#
        );
        let error =
            DescriptorSource::from_json(Path::new("/nix/store/fake-descriptor.json"), &content)
                .unwrap_err();

        assert!(matches!(
            error,
            Error::MissingImageDescriptorField {
                field: "oci_layout",
                ..
            }
        ));
    }

    #[test]
    fn loaded_container_ref_extracts_apple_load_ref() {
        let output = b"loading\nLoaded: untagged@sha256:abcdef0123456789, done\n";
        assert_eq!(
            loaded_container_ref(output, b""),
            Some(String::from("untagged@sha256:abcdef0123456789"))
        );
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
