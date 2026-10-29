mod apple;
pub mod binding;
mod cache;
pub mod compiler;
mod coordination;
pub mod credential;
pub mod deployment;
mod dolt;
pub mod drain;
pub mod generation;
mod managed;
mod owner;
pub mod shared;
pub mod sql;
pub mod supervisor;

use std::{
    env, fs, io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream},
    num::NonZeroU16,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::{
    fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    net::UnixStream,
};

use displaydoc::Display;
use fs2::FileExt;
use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error as ThisError;
use wrix_core::{
    beads_config::{ReadError, read_sync_branch},
    git::Branch,
    path::{ContainerName, Workspace, WorkspaceHash},
};
use wrix_sandbox::image::{
    self as runtime_image, CommandStore, Digest, ImageRef, InstallRequest, Runtime as ImageRuntime,
    Store as _, Target,
};

const SCHEMA_VERSION: u8 = 1;
const CACHE_PORT_START: u16 = 21_000;
const CACHE_PORT_WIDTH: u16 = 2_000;
const DOLT_PORT_START: u16 = 23_000;
const DOLT_PORT_WIDTH: u16 = 2_000;
const CACHE_ENABLED_LABEL: &str = "wrix.cache.enabled";
const CACHE_BACKEND_LABEL: &str = "wrix.cache.backend";
const CACHE_BACKEND_VERSION: &str = "celler-v2";
const SUPERVISION_LABEL: &str = "wrix.service.supervision";
const DOLT_TRANSPORT_LABEL: &str = "wrix.dolt.transport";
const DOLT_AUTH_LABEL: &str = "wrix.dolt.auth";
const DOLT_AUTH_VERSION: &str = "tcp-root-v1";
const DOLT_DISABLED_LABEL_VALUE: &str = "disabled";
const DOLT_READY_TIMEOUT: Duration = Duration::from_secs(6);
const DOLT_READY_INTERVAL: Duration = Duration::from_millis(200);

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// {0}
    SharedDolt(#[from] crate::dolt::Error),
    /// workspace admission failed: {0}
    Admission(#[from] wrix_state::store::Error),
    /// host package service-image binding failed: {0}
    Binding(#[from] binding::Error),
    /// service lifecycle I/O failed: {source}
    Io { source: io::Error },
    /// managed Beads configuration failed: {source}
    Beads { source: managed::Error },
    /// project cache operation failed: {source}
    Cache {
        source: wrix_cache::publisher::Error,
    },
    /// {0}
    ControlDirectory(#[from] wrix_core::private_directory::Error),
    /// {0}
    Coordination(#[from] coordination::Error),
    /// {0}
    SelectedCeller(#[from] owner::Error),
    /// {0}
    CellerLayout(#[from] wrix_cache::backend::layout::Error),
    /// {0}
    CellerRuntime(#[from] wrix_cache::backend::layout::runtime::Error),
    /// {0}
    CellerBootstrap(#[from] cache::Error),
    /// {0}
    Component(#[from] supervisor::Error),
    /// runtime image operation failed: {source}
    Image { source: runtime_image::Error },
    /// environment variable {name} must be valid Unicode
    InvalidUnicodeEnvironment { name: &'static str },
    /// unknown Dolt transport: {value}
    UnknownDoltTransport { value: String },
    /// Dolt TCP port must be nonzero
    ZeroDoltPort,
    /// failed to remove stale Dolt socket {path}: {source}
    StaleDoltSocketRemoval { path: String, source: io::Error },
    /// Dolt endpoint {endpoint} did not become ready within {timeout} seconds: {source}
    DoltEndpointUnavailable {
        endpoint: String,
        timeout: u64,
        source: io::Error,
    },
    /// unknown service image source kind: {value}
    UnknownImageSourceKind { value: String },
    /// invalid service image source: {source}
    ImageSource {
        source: runtime_image::SourceParseError,
    },
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
    /// Celler ownership in container {container} failed: {reason}; explicit host-wide maintenance is required; ordinary startup cannot replace a running backend or start a competing owner; explicit recovery applies to an absent/stopped selected owner using the selected host package
    CellerOwnership {
        container: String,
        reason: OwnershipFailure,
    },
    /// selected Celler owner {container} is {status:?}; run `wrix service recover`
    CellerUnavailable {
        container: String,
        status: RuntimeStatus,
    },
    /// Celler recovery requires an existing selected owner and its recorded runtime; no ownership was transferred
    RecoveryScope,
    /// running legacy service {container} is not a verified shared Dolt/Celler owner; retain it and its peers. Explicitly stop ALL legacy Wrix service containers in both runtimes, then run `wrix service recover` with this host package before retrying; never delete managed data or signing state
    IncompatibleService { container: String },
    /// workspace stop cannot remove shared Celler backend {container}; namespace retirement requires host-wide lifecycle integration, not container removal
    CellerWorkspaceStop { container: String },
    /// service port {port} is already in use by {owner}; stop that process/container or free the port before retrying
    PortInUse { port: u16, owner: String },
    /// no available loopback port in {start}-{end}
    NoAvailableLoopbackPort { start: u16, end: u16 },
    /// invalid persisted service metadata at {path}: {source}
    PersistedServicesJson {
        path: String,
        source: serde_json::Error,
    },
    /// invalid Apple container JSON: {source}
    AppleJson { source: serde_json::Error },
    /// invalid persisted workspace hash: {value}
    InvalidPersistedWorkspaceHash { value: String },
    /// persisted service metadata at {path} must be a regular file, not a symlink or directory
    InvalidRegistrationPath { path: PathBuf },
    /// persisted service metadata at {path} belongs to another workspace
    WorkspaceMismatch { path: PathBuf },
    /// persisted service endpoint at {path} is not loopback-only in its assigned port range
    InvalidPortLease { path: PathBuf },
    /// planned service port {port} is already leased to another workspace
    PortLeaseConflict { port: u16 },
    /// {source}
    BeadsConfig { source: ReadError },
    /// {0}
    CacheConfig(#[from] wrix_cache::config::ReadError),
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

impl From<managed::Error> for Error {
    fn from(source: managed::Error) -> Self {
        Self::Beads { source }
    }
}

impl From<wrix_cache::publisher::Error> for Error {
    fn from(source: wrix_cache::publisher::Error) -> Self {
        Self::Cache { source }
    }
}

impl From<runtime_image::Error> for Error {
    fn from(source: runtime_image::Error) -> Self {
        Self::Image { source }
    }
}

impl From<ReadError> for Error {
    fn from(source: ReadError) -> Self {
        Self::BeadsConfig { source }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheMode {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug)]
pub struct Plan {
    selection: Option<wrix_state::context::Selection>,
    workspace: Workspace,
    beads_sync_branch: Branch,
    paths: Paths,
    container_name: ContainerName,
    cache_port: Option<u16>,
    dolt: Option<DoltEndpoint>,
    selected_owner: Option<owner::Selected>,
    namespace_runtime: RuntimeKind,
    shared: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DoltEndpoint {
    UnixSocket {
        socket_path: DoltSocketPath,
    },
    Tcp {
        socket_path: DoltSocketPath,
        port: NonZeroU16,
    },
}

/// The planned workspace socket, also retained for TCP status diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoltSocketPath(PathBuf);

impl DoltSocketPath {
    pub fn for_workspace(workspace: &Workspace) -> Self {
        Self(workspace.canonical_path().join(".wrix/dolt.sock"))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
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
    host_lifecycle: Option<PathBuf>,
}

struct Registration {
    _host: fs::File,
    _namespace: fs::File,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeStatus {
    Running,
    Stopped,
    Missing,
}

struct PreparedImage(Box<owner::Recipe>);

impl PreparedImage {
    fn as_str(&self) -> &str {
        self.0.image_id.as_str()
    }

    fn clone_recipe(&self) -> owner::Recipe {
        (*self.0).clone()
    }
}

#[derive(Clone, Debug)]
pub struct Status {
    pub runtime: RuntimeStatus,
    pub plan: Plan,
}

impl Plan {
    pub fn for_current_dir(cache_mode: CacheMode) -> Result<Self> {
        let selection = wrix_state::context::Selection::inspect(&env::current_dir()?)?;
        let workspace = selection.workspace().clone();
        let settings = wrix_cache::config::Settings::for_workspace(&workspace)?;
        let paths = Paths::for_workspace(workspace.hash())?;
        let branch = read_sync_branch(workspace.canonical_path())?;
        let mut plan = Self::from_parsed(workspace, cache_mode, paths, branch, &settings)?;
        plan.selection = Some(selection);
        Ok(plan)
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
        let settings = wrix_cache::config::Settings::for_workspace(&workspace)?;
        let beads_sync_branch = read_sync_branch(workspace.canonical_path())?;
        Self::from_parsed(workspace, cache_mode, paths, beads_sync_branch, &settings)
    }

    fn from_parsed(
        workspace: Workspace,
        cache_mode: CacheMode,
        paths: Paths,
        beads_sync_branch: Branch,
        cache_settings: &wrix_cache::config::Settings,
    ) -> Result<Self> {
        let mut container_name = select_container_name(&workspace, &paths)?;
        let prior_ports = PortLease::read(&paths.services_path(), workspace.hash())?;
        let peer_ports = peer_port_leases(&paths, workspace.hash())?;
        let selected_owner = paths
            .host_lifecycle
            .as_deref()
            .map(owner::Selected::read)
            .transpose()?
            .flatten();
        if selected_owner
            .as_ref()
            .is_some_and(|owner| owner.workspace.is_none() && owner.container == container_name)
        {
            container_name = ContainerName::from_persisted(format!("{container_name}-namespace"))
                .ok_or(owner::Error::Invalid)?;
        }
        let shared_record = shared::Record::read(&paths)?;
        if shared_record.is_some() {
            container_name = selected_owner.as_ref().map_or_else(
                || {
                    ContainerName::from_persisted("wrix-host-service".into())
                        .ok_or(owner::Error::Invalid)
                },
                |owner| Ok(owner.container.clone()),
            )?;
        }
        let cache_port = match cache_mode {
            CacheMode::Enabled
                if cache_settings.enable
                    && cache_allowed_for_workspace(workspace.canonical_path()) =>
            {
                Some(match selected_owner.as_ref() {
                    Some(owner) => owner.port.get(),
                    None => select_port(
                        prior_ports.cache_http_port,
                        CACHE_PORT_START,
                        CACHE_PORT_WIDTH,
                        workspace.hash(),
                        &peer_ports,
                    )?,
                })
            }
            CacheMode::Enabled | CacheMode::Disabled => None,
        };
        let dolt = if wrix_core::beads_config::is_configured(workspace.canonical_path()) {
            let transport = DoltTransport::from_env()?;
            let socket_path = DoltSocketPath::for_workspace(&workspace);
            Some(if let Some(shared) = &shared_record {
                shared.endpoint(
                    paths
                        .host_lifecycle
                        .as_deref()
                        .and_then(Path::parent)
                        .ok_or(owner::Error::Invalid)?,
                )
            } else {
                match transport {
                    DoltTransport::UnixSocket => DoltEndpoint::UnixSocket { socket_path },
                    DoltTransport::Tcp => DoltEndpoint::Tcp {
                        socket_path,
                        port: parse_dolt_port(select_port(
                            prior_ports.dolt_tcp_port,
                            DOLT_PORT_START,
                            DOLT_PORT_WIDTH,
                            workspace.hash(),
                            &peer_ports,
                        )?)?,
                    },
                }
            })
        } else {
            None
        };
        Ok(Self {
            selection: None,
            workspace,
            beads_sync_branch,
            paths,
            container_name,
            cache_port,
            dolt,
            selected_owner,
            namespace_runtime: RuntimeKind::current(),
            shared: shared_record.is_some(),
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

    pub fn dolt_port(&self) -> Option<NonZeroU16> {
        self.dolt.as_ref().and_then(DoltEndpoint::tcp_port)
    }

    const fn cache_enabled(&self) -> bool {
        self.cache_port.is_some()
    }

    fn owns_cache(&self) -> bool {
        self.selected_owner
            .as_ref()
            .map_or_else(|| self.cache_enabled(), |owner| owner.owns(self))
    }

    fn owned_cache_port(&self) -> Option<u16> {
        if !self.owns_cache() {
            return None;
        }
        self.selected_owner
            .as_ref()
            .map(|owner| owner.port.get())
            .or(self.cache_port)
    }

    fn cache_container_name(&self) -> ContainerName {
        self.selected_owner
            .as_ref()
            .map_or_else(|| self.container_name(), |owner| owner.container.clone())
    }

    fn reconcile_owner(&mut self, guard: &coordination::Guard) -> Result<()> {
        let selected = owner::Selected::read(guard.root())?;
        if self.selected_owner.is_some() && selected.is_none() {
            return Err(owner::Error::Conflict.into());
        }
        if self.cache_enabled()
            && let Some(owner) = &selected
        {
            self.cache_port = Some(owner.port.get());
        }
        self.selected_owner = selected;
        Ok(())
    }

    #[cfg(test)]
    fn has_services(&self) -> bool {
        self.owns_cache() || self.dolt.is_some()
    }

    fn selected_host_ports(&self) -> Vec<u16> {
        let mut ports = Vec::new();
        if let Some(port) = self.owned_cache_port() {
            ports.push(port);
        }
        if let Some(port) = self.dolt_port() {
            ports.push(port.get());
        }
        ports
    }

    fn authorize(&self) -> Result<()> {
        if let Some(selection) = &self.selection {
            return Ok(selection.authorize_effects()?);
        }
        if wrix_state::namespace::is_host_root(self.paths.state_root())? {
            let selection =
                wrix_state::context::Selection::inspect(self.workspace.canonical_path())?;
            if selection.workspace().hash() != self.workspace.hash()
                || selection.workspace().canonical_path().as_os_str()
                    != self.workspace.canonical_path().as_os_str()
            {
                return Err(wrix_state::store::Error::Conflict.into());
            }
            selection.authorize_effects()?;
        }
        Ok(())
    }

    pub fn ensure_layout(&self) -> Result<()> {
        self.authorize()?;
        self.validate_registration()?;
        let _lock = self.paths.lock_registration()?;
        self.ensure_layout_locked()
    }

    fn validate_registration(&self) -> Result<()> {
        PortLease::read(&self.paths.services_path(), self.workspace.hash())?;
        if shared::is_managed(self)? {
            return Ok(());
        }
        let peer_ports = peer_port_leases(&self.paths, self.workspace.hash())?;
        for port in self.selected_host_ports() {
            if peer_ports.contains(&port) {
                return Err(Error::PortLeaseConflict { port });
            }
        }
        Ok(())
    }

    fn ensure_layout_locked(&self) -> Result<()> {
        self.authorize()?;
        self.validate_registration()?;
        wrix_core::private_directory::prepare_with_parents(self.paths.state_root())?;
        if self.cache_enabled() {
            if self.selected_owner.is_none() {
                let layout = wrix_cache::backend::layout::Layout::current()?;
                layout.prepare()?;
                wrix_cache::backend::layout::runtime::install(layout.metadata())?;
            }
            if !wrix_state::namespace::is_host_root(self.paths.state_root())? {
                fs::create_dir_all(self.paths.gcroots_dir())?;
                fs::create_dir_all(self.paths.pending_dir())?;
            }
            fs::create_dir_all(self.paths.keys_dir())?;
            write_if_missing(&self.paths.cache_lock_path(), "")?;
            write_if_missing(
                &self.paths.cache_status_path(),
                default_cache_status().as_bytes(),
            )?;
            write_if_missing(&self.paths.publish_roots_path(), b"{\n  \"roots\": []\n}\n")?;
        }
        if self.dolt.is_some() {
            fs::create_dir_all(self.workspace.canonical_path().join(".wrix"))?;
        }
        self.write_services()
    }

    #[cfg(test)]
    fn beads_worktree_remote(&self) -> Option<(&Branch, PathBuf)> {
        let branch = &self.beads_sync_branch;
        let path = self
            .workspace
            .canonical_path()
            .join(".git/beads-worktrees")
            .join(branch.as_str())
            .join(".beads/dolt-remote");
        path.is_dir().then_some((branch, path))
    }

    fn write_services(&self) -> Result<()> {
        self.authorize()?;
        let bytes = self.services_json();
        if matches!(
            wrix_state::namespace::write_at(&self.paths.services_path(), bytes.as_bytes())?,
            wrix_state::namespace::Storage::Filesystem
        ) {
            managed::atomic_write(&self.paths.services_path(), bytes.as_bytes())?;
        }
        Ok(())
    }

    pub fn services_json(&self) -> String {
        self.render_services_json(&json_port(self.cache_port))
    }

    fn render_services_json(&self, cache_http: &str) -> String {
        let dolt = json_dolt_endpoint(self.dolt.as_ref());
        let dolt_unix = json_dolt_unix(self.dolt.as_ref());
        let dolt_tcp = json_dolt_tcp(self.dolt.as_ref());
        format!(
            concat!(
                "{{\n",
                "  \"schema_version\": {},\n",
                "  \"dolt_managed\": {},\n",
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
            self.shared,
            escape_json(&self.workspace.canonical_path().display().to_string()),
            self.workspace.hash(),
            escape_json(self.container_name().as_str()),
            escape_json(&self.paths.state_root().display().to_string()),
            escape_json(&self.paths.cache_root().display().to_string()),
            cache_http,
            dolt,
            dolt_unix,
            dolt_tcp
        )
    }
}

impl DoltEndpoint {
    pub const fn transport(&self) -> DoltTransport {
        match self {
            Self::UnixSocket { .. } => DoltTransport::UnixSocket,
            Self::Tcp { .. } => DoltTransport::Tcp,
        }
    }

    pub fn socket_path(&self) -> &Path {
        match self {
            Self::UnixSocket { socket_path } | Self::Tcp { socket_path, .. } => {
                socket_path.as_path()
            }
        }
    }

    pub const fn tcp_port(&self) -> Option<NonZeroU16> {
        match self {
            Self::UnixSocket { .. } => None,
            Self::Tcp { port, .. } => Some(*port),
        }
    }

    pub const fn tcp_host(&self) -> Option<&'static str> {
        match self {
            Self::UnixSocket { .. } => None,
            Self::Tcp { .. } => Some("127.0.0.1"),
        }
    }
}

fn parse_dolt_port(port: u16) -> Result<NonZeroU16> {
    NonZeroU16::new(port).ok_or(Error::ZeroDoltPort)
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
        let state_root = state_root.into();
        Self {
            state_root,
            cache_root: cache_root.into(),
            host_lifecycle: None,
        }
    }

    fn for_workspace(hash: &WorkspaceHash) -> Result<Self> {
        let roots = wrix_core::host_path::Roots::discover()?;
        let state_root = roots.workspace_state(hash);
        let cache_root = roots.workspace_cache(hash);
        let host_lifecycle = roots.state().join("host-lifecycle");
        Ok(Self {
            state_root,
            cache_root,
            host_lifecycle: Some(host_lifecycle),
        })
    }

    pub fn state_root(&self) -> &Path {
        self.state_root.as_path()
    }

    fn state_base(&self) -> Option<&Path> {
        self.state_root.parent()
    }

    fn lock_registration(&self) -> Result<Registration> {
        let base = self.state_base().ok_or_else(|| Error::Operation {
            message: String::from("workspace state root has no parent"),
        })?;
        fs::create_dir_all(base)?;
        let host = lock_registration_file(base, "service-registration.lock")?;
        wrix_core::private_directory::prepare_with_parents(self.state_root())?;
        let namespace = lock_registration_file(self.state_root(), "service.lock")?;
        Ok(Registration {
            _host: host,
            _namespace: namespace,
        })
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

fn lock_registration_file(root: &Path, name: &str) -> Result<fs::File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(name))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != fs::metadata(root)?.uid() {
        return Err(Error::Operation {
            message: "service registration lock is not an owned single-link regular file".into(),
        });
    }
    file.lock_exclusive()?;
    Ok(file)
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
            option_port(self.plan.dolt_port().map(NonZeroU16::get)),
            self.runtime
        )
    }
}

#[derive(Debug, Display)]
pub enum OwnershipFailure {
    /// selected owner belongs to a different container runtime
    Runtime,
    /// selected running owner's identity, backend version, endpoint or image is incompatible
    Identity,
    /// running owner's service features are incompatible with the workspace plan
    Features,
    /// competing running cache backend was found in the runtime inventory
    Competing,
}

/// Recover the host-user cache backend without requiring a repository.
pub fn recover() -> Result<String> {
    owner::host::recover()
}

pub fn repair() -> Result<String> {
    owner::host::repair()
}

pub fn rename(name: &ContainerName) -> Result<String> {
    owner::rename::run(name)
}

pub fn start(cache_mode: CacheMode) -> Result<Status> {
    let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
    let (mut plan, _lock) = locked_plan(cache_mode)?;
    plan.authorize()?;
    deployment::resume(&host)?;
    let _pools = crate::nix_store::lifecycle::gate::shared(&host).map_err(io::Error::other)?;
    supervisor::transition::ensure_clear(plan.paths().state_root(), plan.workspace().hash())?;
    let host_lock = host_lifecycle_lock(plan.paths())?;
    let guard = &host_lock;
    plan.reconcile_owner(guard)?;
    let runtime = Runtime::from_env()?;
    runtime.binding.as_ref().ok_or(binding::Error::Missing)?;
    generation::preflight_controller(&host)?;
    let legacy_namespace = if plan.selected_owner.is_some() && shared::is_managed(&plan)? {
        None
    } else {
        Some(plan.workspace().hash())
    };
    shared::check_inventory(&runtime, plan.selected_owner.as_ref(), legacy_namespace)?;
    let selected = owner::host::ensure(&runtime, guard, plan.selected_owner.as_ref(), None, false)?;
    plan.reconcile_owner(guard)?;
    plan.container_name = selected.container;
    plan.shared = true;
    let record = shared::Record::read(plan.paths())?.ok_or(owner::Error::Invalid)?;
    if plan.dolt.is_some() {
        plan.dolt = Some(record.endpoint(shared::host_root(&plan)?));
        managed::configure(plan.workspace().canonical_path())?;
        shared::register(&plan, &runtime)?;
    }
    if plan.cache_enabled() {
        let prepared = cache::prepare(&plan, RuntimeStatus::Running)?;
        cache::activate(&plan, &prepared)?;
    }
    plan.ensure_layout_locked()?;
    status_for_plan(plan)
}

/// Rotates only the admitted workspace's private view on the verified selected running backend.
pub fn rotate_cache_key() -> Result<()> {
    wrix_state::host::Host::discover().map_err(io::Error::other)?;
    let (mut plan, _registration) = locked_plan(CacheMode::Enabled)?;
    plan.authorize()?;
    let guard = host_lifecycle_lock(plan.paths())?;
    plan.reconcile_owner(&guard)?;
    supervisor::transition::ensure_clear(plan.paths().state_root(), plan.workspace().hash())?;
    if plan.selected_owner.is_none() || !plan.cache_enabled() {
        return Err(Error::Operation {
            message: "cache rotation requires a selected running cache backend and enabled workspace cache".into(),
        });
    }
    let runtime = Runtime::from_env()?;
    runtime.validate_selected_owner(&plan)?;
    shared::check_inventory(&runtime, plan.selected_owner.as_ref(), None)?;
    cache::rotate(&plan)?;
    Ok(())
}

fn locked_plan(cache_mode: CacheMode) -> Result<(Plan, Registration)> {
    let selection = wrix_state::context::Selection::inspect(&env::current_dir()?)?;
    let workspace = selection.workspace().clone();
    let settings = wrix_cache::config::Settings::for_workspace(&workspace)?;
    let paths = Paths::for_workspace(workspace.hash())?;
    let branch = read_sync_branch(workspace.canonical_path())?;
    Plan::from_parsed(
        workspace.clone(),
        cache_mode,
        paths.clone(),
        branch.clone(),
        &settings,
    )?;
    selection.authorize_effects()?;
    let lock = paths.lock_registration()?;
    selection.authorize_effects()?;
    let mut plan = Plan::from_parsed(workspace, cache_mode, paths, branch, &settings)?;
    plan.selection = Some(selection);
    Ok((plan, lock))
}

fn host_lifecycle_lock(paths: &Paths) -> Result<coordination::Guard> {
    let root = paths
        .host_lifecycle
        .as_deref()
        .ok_or_else(|| Error::Operation {
            message: "missing host state root for lifecycle coordination".into(),
        })?;
    Ok(coordination::acquire(root)?)
}

pub fn stop(cache_mode: CacheMode) -> Result<Status> {
    wrix_state::host::Host::discover().map_err(io::Error::other)?;
    let (mut plan, _lock) = locked_plan(cache_mode)?;
    let host_lock = host_lifecycle_lock(plan.paths())?;
    plan.reconcile_owner(&host_lock)?;
    supervisor::transition::ensure_clear(plan.paths().state_root(), plan.workspace().hash())?;
    let runtime = Runtime::from_env()?;
    plan.authorize()?;
    if shared::is_managed(&plan)? {
        runtime.validate_selected_owner(&plan)?;
        if plan.dolt().is_some() {
            shared::retire(&plan, &runtime)?;
        }
        cache::retire(&plan)?;
        return Ok(Status {
            runtime: RuntimeStatus::Stopped,
            plan,
        });
    }
    if plan.selected_owner.is_some() && !plan.owns_cache() {
        runtime.ensure_workspace_stop_safe(&plan)?;
        cache::retire(&plan)?;
        if runtime.status(&plan.container_name())? != RuntimeStatus::Missing {
            runtime.remove(&plan.container_name())?;
        }
        return status_for_plan(plan);
    }
    if plan.owns_cache()
        && plan.dolt().is_some()
        && runtime.status(&plan.container_name())? == RuntimeStatus::Running
        && runtime.supervision(&plan.container_name())? == Some(supervisor::Version::Components)
    {
        runtime.ensure_cache_owner_compatible(&plan)?;
        cache::retire(&plan)?;
        let pending = supervisor::transition::Pending::begin(
            &host_lock,
            plan.paths().state_root(),
            plan.workspace().hash(),
            supervisor::transition::Change::Retire,
        )?;
        pending.finish(runtime.component_observation(&plan, supervisor::Action::Retire)?)?;
        return status_for_plan(plan);
    }
    if plan.dolt().is_none()
        && runtime.status(&plan.container_name())? == RuntimeStatus::Running
        && runtime
            .inspect_label(plan.container_name().as_str(), CACHE_BACKEND_LABEL)?
            .as_deref()
            == Some(CACHE_BACKEND_VERSION)
        && runtime.running_container_satisfies_plan(&plan.container_name(), &plan)?
    {
        cache::retire(&plan)?;
        return Ok(Status {
            runtime: RuntimeStatus::Stopped,
            plan,
        });
    }
    runtime.ensure_workspace_stop_safe(&plan)?;
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

/// Project cache metadata for sandbox reads; host publication and durable metadata are unchanged.
pub fn sandbox_cache_endpoints(cache_mode: CacheMode) -> Result<String> {
    let plan = Plan::for_current_dir(cache_mode)?;
    let Some(port) = plan.cache_port() else {
        return Ok(plan.services_json());
    };
    let endpoint = Runtime::from_env()?.sandbox_endpoint(&plan, SandboxService::Cache, port)?;
    Ok(plan.render_services_json(&json_tcp_endpoint(endpoint)))
}

/// Resolve a TCP endpoint in the sandbox's network, not the host's loopback namespace.
pub fn sandbox_dolt_endpoint(cache_mode: CacheMode) -> Result<String> {
    let plan = Plan::for_current_dir(cache_mode)?;
    let service = SandboxService::Dolt;
    let port = plan.dolt_port().ok_or_else(|| service.unavailable())?;
    let endpoint = Runtime::from_env()?.sandbox_endpoint(&plan, service, port.get())?;
    Ok(json_tcp_endpoint(endpoint))
}

#[derive(Clone, Copy)]
enum SandboxService {
    Cache,
    Dolt,
    Sync,
}

impl SandboxService {
    const fn internal_port(self) -> u16 {
        match self {
            Self::Cache => 8080,
            Self::Dolt => 3306,
            Self::Sync => 3307,
        }
    }

    fn unavailable(self) -> Error {
        let name = match self {
            Self::Cache => "project cache",
            Self::Dolt => "Dolt",
            Self::Sync => "Beads sync",
        };
        Error::Operation {
            message: format!(
                "no sandbox-reachable {name} TCP endpoint; start the workspace service and check its runtime network"
            ),
        }
    }
}

fn json_tcp_endpoint(endpoint: SocketAddrV4) -> String {
    format!(
        "{{\"host\":\"{}\",\"port\":{}}}",
        endpoint.ip(),
        endpoint.port()
    )
}

pub fn wait_for_dolt(cache_mode: CacheMode) -> Result<()> {
    let plan = Plan::for_current_dir(cache_mode)?;
    if shared::is_managed(&plan)? {
        return shared::probe_client();
    }
    wait_for_dolt_plan(&plan)
}

fn status_for_plan(plan: Plan) -> Result<Status> {
    if shared::is_managed(&plan)? {
        let retired = if plan.dolt().is_some() {
            !shared::sql_active(&plan)?
        } else {
            cache::is_retired(&plan)?
        };
        let runtime = if retired {
            RuntimeStatus::Stopped
        } else {
            Runtime::from_env()?.status(&plan.container_name())?
        };
        return Ok(Status { runtime, plan });
    }
    supervisor::transition::ensure_clear(plan.paths().state_root(), plan.workspace().hash())?;
    let runtime = if plan.owns_cache() && plan.dolt().is_some() && cache::is_retired(&plan)? {
        let runtime = Runtime::from_env()?;
        runtime.validate_selected_owner(&plan)?;
        let status = runtime.status(&plan.container_name())?;
        if status == RuntimeStatus::Running
            && runtime.supervision(&plan.container_name())? == Some(supervisor::Version::Components)
            && runtime.component_observation(&plan, supervisor::Action::Inspect)?
                == supervisor::Observation::Retired
        {
            RuntimeStatus::Stopped
        } else {
            status
        }
    } else if plan.dolt().is_none() && cache::is_retired(&plan)? {
        RuntimeStatus::Stopped
    } else if plan.dolt().is_none() && plan.cache_enabled() && plan.selected_owner.is_some() {
        let runtime = Runtime::from_env()?;
        runtime.validate_selected_owner(&plan)?;
        runtime.status(&plan.cache_container_name())?
    } else {
        Runtime::from_env()?.status(&plan.container_name())?
    };
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
    fn scoped_port(&self, start: u16, width: u16, path: &Path) -> Result<u16> {
        let port = self.port.get();
        if self.host != Ipv4Addr::LOCALHOST || !(start..start + width).contains(&port) {
            return Err(Error::InvalidPortLease {
                path: path.to_path_buf(),
            });
        }
        Ok(port)
    }
}

impl PortLease {
    fn read(path: &Path, workspace_hash: &WorkspaceHash) -> Result<Self> {
        let Some(services) = read_persisted_services(path)? else {
            return Ok(Self::default());
        };
        if !services.workspace_hash.matches(workspace_hash) {
            return Err(Error::WorkspaceMismatch {
                path: path.to_path_buf(),
            });
        }
        Self::from_endpoints(&services.endpoints, path)
    }

    fn from_endpoints(endpoints: &PersistedEndpoints, path: &Path) -> Result<Self> {
        Ok(Self {
            cache_http_port: endpoints
                .cache_http
                .as_ref()
                .map(|endpoint| endpoint.scoped_port(CACHE_PORT_START, CACHE_PORT_WIDTH, path))
                .transpose()?,
            dolt_tcp_port: endpoints
                .dolt_tcp
                .as_ref()
                .map(|endpoint| endpoint.scoped_port(DOLT_PORT_START, DOLT_PORT_WIDTH, path))
                .transpose()?,
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
    let content = match wrix_state::namespace::read_at(path)? {
        wrix_state::namespace::Source::Host(Some(content)) => content,
        wrix_state::namespace::Source::Host(None) => return Ok(None),
        wrix_state::namespace::Source::Observer => {
            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            };
            if !metadata.is_file() {
                return Err(Error::InvalidRegistrationPath {
                    path: path.to_path_buf(),
                });
            }
            fs::read(path)?
        }
    };
    serde_json::from_slice(&content)
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
    if let Some(entries) = wrix_state::namespace::services_at(paths.state_root())? {
        for entry in entries {
            let services: PersistedServices = serde_json::from_slice(&entry.bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if &entry.key != workspace_hash && &services.container_name == candidate {
                return Ok(true);
            }
        }
        return Ok(false);
    }
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeKind {
    Podman,
    Container,
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
    binding: Option<binding::Candidate>,
}

impl RuntimeKind {
    fn current() -> Self {
        Self::for_binary(&env::var("WRIX_CONTAINER_RUNTIME").unwrap_or_else(|_| default_runtime()))
    }

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

impl ContainerInfo {
    fn belongs_to_workspace(&self, plan: &Plan) -> bool {
        let workspace_path = plan.workspace().canonical_path().display().to_string();
        self.workspace_hash.as_deref().map_or_else(
            || self.workspace_path.as_deref() == Some(workspace_path.as_str()),
            |hash| hash == plan.workspace().hash().as_str(),
        )
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
        let binding = env::var_os("WRIX_SERVICE_PACKAGE_BINDING")
            .map(|_| binding::Candidate::from_env())
            .transpose()?;
        Ok(Self {
            kind: RuntimeKind::for_binary(&binary),
            binary,
            binding,
        })
    }

    fn sandbox_endpoint(
        &self,
        plan: &Plan,
        service: SandboxService,
        host_port: u16,
    ) -> Result<SocketAddrV4> {
        if matches!(service, SandboxService::Cache) {
            self.validate_selected_owner(plan)?;
        }
        match self.kind {
            RuntimeKind::Container => {
                if shared::Record::read(plan.paths())?
                    .is_some_and(|record| record.native_ports().is_some())
                {
                    let role = match service {
                        SandboxService::Dolt => crate::route::Role::Sql,
                        SandboxService::Cache => crate::route::Role::CacheRead,
                        SandboxService::Sync => crate::route::Role::Sync,
                    };
                    return generation::guest_endpoint(shared::host_root(plan)?, role);
                }
                let snapshot = self
                    .apple_snapshot(
                        match service {
                            SandboxService::Cache => plan.cache_container_name(),
                            SandboxService::Dolt | SandboxService::Sync => plan.container_name(),
                        }
                        .as_str(),
                    )?
                    .ok_or_else(|| service.unavailable())?;
                if snapshot.runtime_status() != RuntimeStatus::Running {
                    return Err(service.unavailable());
                }
                let host = snapshot
                    .ipv4_address()
                    .ok_or_else(|| service.unavailable())?;
                Ok(SocketAddrV4::new(host, service.internal_port()))
            }
            RuntimeKind::Podman if cfg!(target_os = "linux") => {
                Ok(SocketAddrV4::new(Ipv4Addr::new(169, 254, 1, 2), host_port))
            }
            RuntimeKind::Podman => Err(service.unavailable()),
        }
    }

    fn ensure_workspace_stop_safe(&self, plan: &Plan) -> Result<()> {
        let name = plan.container_name();
        let status = self.status(&name)?;
        if status != RuntimeStatus::Missing
            && self
                .inspect_label(name.as_str(), CACHE_BACKEND_LABEL)?
                .is_some()
        {
            return Err(Error::CellerWorkspaceStop {
                container: name.as_str().to_owned(),
            });
        }
        if status != RuntimeStatus::Missing && !self.namespace_identity_matches(plan)? {
            return Err(Error::IncompatibleService {
                container: name.as_str().into(),
            });
        }
        Ok(())
    }

    fn native_cache_port(&self, name: &ContainerName, selected: NonZeroU16) -> Result<NonZeroU16> {
        self.inspect_label(name.as_str(), "wrix.cache.native-port")?
            .map_or_else(
                || Ok(selected),
                |port| {
                    port.parse().map_err(|_| Error::Operation {
                        message: "native cache port label is invalid".into(),
                    })
                },
            )
    }

    fn validate_selected_owner(&self, plan: &Plan) -> Result<()> {
        let Some(owner) = &plan.selected_owner else {
            return Ok(());
        };
        if owner.runtime != self.kind {
            return Err(Error::CellerOwnership {
                container: owner.container.as_str().into(),
                reason: OwnershipFailure::Runtime,
            });
        }
        let status = self.status(&owner.container)?;
        if status != RuntimeStatus::Running {
            return Err(Error::CellerUnavailable {
                container: owner.container.as_str().into(),
                status,
            });
        }
        if (owner.workspace.is_some()
            && owner.container == plan.container_name()
            && !owner.owns(plan))
            || self
                .inspect_label(owner.container.as_str(), "wrix.kind")?
                .as_deref()
                != Some("service")
            || (owner.workspace.is_some()
                && self
                    .inspect_label(owner.container.as_str(), "wrix.workspace.hash")?
                    .as_deref()
                    != owner.workspace.as_ref().map(WorkspaceHash::as_str))
            || (owner.workspace.is_none()
                && self
                    .inspect_label(owner.container.as_str(), "wrix.service.scope")?
                    .as_deref()
                    != Some("host"))
            || self
                .inspect_label(owner.container.as_str(), CACHE_BACKEND_LABEL)?
                .as_deref()
                != Some(CACHE_BACKEND_VERSION)
            || self
                .inspect_label(owner.container.as_str(), CACHE_ENABLED_LABEL)?
                .as_deref()
                != Some("true")
            || !self
                .published_ports(owner.container.as_str())?
                .contains(&self.native_cache_port(&owner.container, owner.port)?.get())
            || (owner.recipe.is_some()
                && self.container_image_id(&owner.container)?
                    != owner.recipe.as_ref().map(|recipe| recipe.image_id.clone()))
        {
            return Err(Error::CellerOwnership {
                container: owner.container.as_str().into(),
                reason: OwnershipFailure::Identity,
            });
        }
        Ok(())
    }

    fn ensure_cache_owner_compatible(&self, plan: &Plan) -> Result<()> {
        if plan.cache_enabled() || plan.owns_cache() {
            self.validate_selected_owner(plan)?;
            let alternate = self.alternate();
            if let Some(containers) = alternate.available_container_infos()? {
                alternate.ensure_cache_inventory_compatible(plan, containers, false)?;
            }
        }
        self.ensure_cache_inventory_compatible(plan, self.list_container_infos()?, true)
    }

    fn alternate(&self) -> Self {
        let (kind, binary) = match self.kind {
            RuntimeKind::Podman => (RuntimeKind::Container, "container"),
            RuntimeKind::Container => (RuntimeKind::Podman, "podman"),
        };
        Self {
            kind,
            binary: binary.to_owned(),
            binding: None,
        }
    }

    fn ensure_cache_inventory_compatible(
        &self,
        plan: &Plan,
        containers: Vec<ContainerInfo>,
        selected_runtime: bool,
    ) -> Result<()> {
        for container in containers {
            if container.kind.as_deref() != Some("service") {
                continue;
            }
            let selected = selected_runtime && container.name == plan.container_name().as_str();
            if !selected && !plan.cache_enabled() && !plan.owns_cache() {
                continue;
            }
            if selected_runtime
                && let Some(owner) = &plan.selected_owner
                && owner.runtime == self.kind
                && container.name == owner.container.as_str()
            {
                if selected
                    && self.status(&owner.container)? == RuntimeStatus::Running
                    && !self.running_container_satisfies_plan(&owner.container, plan)?
                {
                    return Err(Error::CellerOwnership {
                        container: container.name,
                        reason: OwnershipFailure::Features,
                    });
                }
                continue;
            }
            let backend = self.inspect_label(&container.name, CACHE_BACKEND_LABEL)?;
            if backend.is_none()
                || self
                    .inspect_label(&container.name, CACHE_ENABLED_LABEL)?
                    .as_deref()
                    == Some("false")
                || self.status_identifier(&container.name)? != RuntimeStatus::Running
            {
                continue;
            }
            if selected
                && backend.as_deref() == Some(CACHE_BACKEND_VERSION)
                && self.running_container_satisfies_plan(&plan.container_name(), plan)?
            {
                continue;
            }
            return Err(Error::CellerOwnership {
                container: container.name,
                reason: OwnershipFailure::Competing,
            });
        }
        Ok(())
    }

    fn namespace_identity_matches(&self, plan: &Plan) -> Result<bool> {
        let name = plan.container_name();
        let identity = ContainerInfo {
            name: name.as_str().into(),
            kind: self.inspect_label(name.as_str(), "wrix.kind")?,
            workspace_hash: self.inspect_label(name.as_str(), "wrix.workspace.hash")?,
            workspace_path: self.inspect_label(name.as_str(), "wrix.workspace")?,
            published_ports: Vec::new(),
        };
        Ok(identity.kind.as_deref() == Some("service") && identity.belongs_to_workspace(plan))
    }

    fn supervision(&self, name: &ContainerName) -> Result<Option<supervisor::Version>> {
        Ok(self
            .inspect_label(name.as_str(), SUPERVISION_LABEL)?
            .as_deref()
            .and_then(supervisor::Version::parse))
    }

    fn component_observation(
        &self,
        plan: &Plan,
        action: supervisor::Action,
    ) -> Result<supervisor::Observation> {
        let name = plan.container_name();
        let mut command = Command::new(&self.binary);
        command.args([
            "exec",
            name.as_str(),
            "wrix-service-supervisor",
            action.as_str(),
            supervisor::DIRECTORY,
            plan.workspace().hash().as_str(),
        ]);
        Ok(supervisor::transport::execute(command)?)
    }

    fn running_container_satisfies_plan(&self, name: &ContainerName, plan: &Plan) -> Result<bool> {
        if !self.namespace_identity_matches(plan)? {
            return Ok(false);
        }
        if plan.owns_cache()
            && (self
                .inspect_label(name.as_str(), CACHE_BACKEND_LABEL)?
                .as_deref()
                != Some(CACHE_BACKEND_VERSION)
                || self
                    .inspect_label(name.as_str(), "wrix.workspace.hash")?
                    .as_deref()
                    != Some(plan.workspace().hash().as_str()))
        {
            return Ok(false);
        }
        if plan.owns_cache()
            && self
                .inspect_label(name.as_str(), CACHE_ENABLED_LABEL)?
                .as_deref()
                != Some("true")
        {
            return Ok(false);
        }
        let supervision = if plan.owns_cache() && plan.dolt().is_some() {
            self.supervision(name)?
        } else {
            None
        };
        if plan.owns_cache() && plan.dolt().is_some() && supervision.is_none() {
            return Ok(false);
        }
        if plan.selected_owner.is_some()
            && !plan.owns_cache()
            && self
                .inspect_label(name.as_str(), CACHE_ENABLED_LABEL)?
                .as_deref()
                == Some("true")
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
        if let Some(dolt) = plan.dolt() {
            match dolt.transport() {
                DoltTransport::UnixSocket if !is_unix_socket(dolt.socket_path())? => {
                    if supervision != Some(supervisor::Version::Components)
                        || !matches!(
                            self.component_observation(plan, supervisor::Action::Inspect)?,
                            supervisor::Observation::Active
                                | supervisor::Observation::Retiring
                                | supervisor::Observation::Retired
                        )
                    {
                        return Ok(false);
                    }
                }
                DoltTransport::Tcp
                    if self
                        .inspect_label(name.as_str(), DOLT_AUTH_LABEL)?
                        .as_deref()
                        != Some(DOLT_AUTH_VERSION) =>
                {
                    return Ok(false);
                }
                DoltTransport::UnixSocket | DoltTransport::Tcp => {}
            }
        }
        let published_ports = self.published_ports(name.as_str())?;
        Ok(plan
            .selected_host_ports()
            .iter()
            .all(|port| published_ports.contains(port)))
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
        self.available_container_infos()?
            .ok_or_else(|| Error::Operation {
                message: format!("container runtime {} is unavailable", self.binary),
            })
    }

    fn inventory_binary(&self) -> io::Result<Option<PathBuf>> {
        let binary = Path::new(&self.binary);
        let candidates = if binary.is_absolute() || binary.components().count() > 1 {
            vec![binary.to_path_buf()]
        } else if let Some(path) = env::var_os("PATH") {
            env::split_paths(&path)
                .map(|root| root.join(binary))
                .collect()
        } else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "container runtime search requires PATH",
            ));
        };
        for path in candidates {
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    return Ok(Some(if path.is_absolute() {
                        path
                    } else {
                        env::current_dir()?.join(path)
                    }));
                }
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(source),
            }
        }
        Ok(None)
    }

    fn available_container_infos(&self) -> Result<Option<Vec<ContainerInfo>>> {
        let args: &[&str] = match self.kind {
            RuntimeKind::Container => &["list", "--all", "--format", "json"],
            RuntimeKind::Podman => &["ps", "-a", "--format", "{{.Names}}"],
        };
        let Some(binary) = self.inventory_binary()? else {
            return Ok(None);
        };
        let output = Command::new(binary)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(Error::Operation {
                message: format!(
                    "failed to list containers in {}: {}{}",
                    self.binary,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        if self.kind == RuntimeKind::Container {
            let snapshots: Vec<apple::Snapshot> = serde_json::from_slice(&output.stdout)
                .map_err(|source| Error::AppleJson { source })?;
            return Ok(Some(
                snapshots
                    .into_iter()
                    .map(apple::Snapshot::into_info)
                    .collect(),
            ));
        }
        let names: Vec<_> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
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
        Ok(Some(containers))
    }

    fn apple_snapshots(&self, args: &[&str]) -> Result<Vec<apple::Snapshot>> {
        let output = Command::new(&self.binary)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            let message = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if args.first() == Some(&"inspect") && is_missing_container_remove_error(&message) {
                return Ok(Vec::new());
            }
            return Err(Error::Operation { message });
        }
        serde_json::from_slice(&output.stdout).map_err(|source| Error::AppleJson { source })
    }

    fn apple_snapshot(&self, name: &str) -> Result<Option<apple::Snapshot>> {
        Ok(self
            .apple_snapshots(&["inspect", name])?
            .into_iter()
            .find(|snapshot| snapshot.configuration.id == name))
    }

    fn container_image_id(&self, name: &ContainerName) -> Result<Option<Digest>> {
        if self.kind == RuntimeKind::Container {
            return Ok(self
                .apple_snapshot(name.as_str())?
                .and_then(|snapshot| snapshot.image_id().cloned()));
        }
        self.inspect_format(name.as_str(), "{{.Image}}")?
            .as_deref()
            .map(parse_image_id)
            .transpose()
    }

    fn inspect_label(&self, name: &str, label: &str) -> Result<Option<String>> {
        if self.kind == RuntimeKind::Container {
            return Ok(self
                .apple_snapshot(name)?
                .and_then(|mut snapshot| snapshot.configuration.labels.remove(label)));
        }
        let template = format!("{{{{ index .Config.Labels \"{label}\" }}}}");
        self.inspect_format(name, &template)
    }

    fn inspect_format(&self, name: &str, template: &str) -> Result<Option<String>> {
        let output = Command::new(&self.binary)
            .arg("inspect")
            .arg("--format")
            .arg(template)
            .arg(name)
            .arg("--type=container")
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
        if self.kind == RuntimeKind::Container {
            return Ok(self
                .apple_snapshot(name)?
                .map(|snapshot| snapshot.into_info().published_ports)
                .unwrap_or_default());
        }
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

    fn ensure_image_at(&self, root: &Path) -> Result<PreparedImage> {
        let candidate = self.binding.as_ref().ok_or(binding::Error::Missing)?;
        let roots = root.join("service-image-roots");
        let digest = candidate.realize(&roots)?;
        let runtime = match self.kind {
            RuntimeKind::Podman => ImageRuntime::Podman,
            RuntimeKind::Container => ImageRuntime::Container,
        };
        let mut store = CommandStore;
        runtime_image::install(
            &mut store,
            &InstallRequest::new(
                runtime,
                candidate.image(),
                candidate.source(),
                Some(&digest),
            )?,
        )?;
        if self.image_exists(candidate.image())? {
            let target = Target::Reference(candidate.image().clone());
            let image_id = store
                .image_id(runtime, &target)?
                .ok_or_else(|| Error::Operation {
                    message: "installed service image has no immutable native identity".into(),
                })?;
            let image_id = parse_image_id(image_id.as_str())?;
            let verified = match self.kind {
                RuntimeKind::Podman => image_id == digest,
                RuntimeKind::Container => {
                    store.image_digest(runtime, &target)?.as_ref() == Some(&digest)
                }
            };
            if !verified {
                return Err(Error::Operation {
                    message: "installed service image does not match the bound content digest"
                        .into(),
                });
            }
            Ok(PreparedImage(Box::new(owner::Recipe {
                binding: candidate.clone(),
                image_id,
                config_digest: digest,
            })))
        } else {
            Err(Error::InstalledImageUnavailable {
                path: candidate.source().path().display().to_string(),
                image: candidate.image().as_str().to_owned(),
            })
        }
    }

    fn image_exists(&self, image: &ImageRef) -> Result<bool> {
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
            .arg(image.as_str())
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
        self.status_identifier(name.as_str())
    }

    fn status_identifier(&self, name: &str) -> Result<RuntimeStatus> {
        if self.kind == RuntimeKind::Container {
            return Ok(self
                .apple_snapshot(name)?
                .as_ref()
                .map_or(RuntimeStatus::Missing, apple::Snapshot::runtime_status));
        }
        let output = Command::new(&self.binary)
            .args([
                "inspect",
                "--format",
                "{{.State.Running}}",
                name,
                "--type=container",
            ])
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            let message = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if is_missing_container_remove_error(&message) {
                return Ok(RuntimeStatus::Missing);
            }
            return Err(Error::Operation { message });
        }
        match String::from_utf8_lossy(&output.stdout).trim() {
            "true" => Ok(RuntimeStatus::Running),
            "false" => Ok(RuntimeStatus::Stopped),
            _ => Err(Error::Operation {
                message: format!("container {name} returned an unknown running state"),
            }),
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

fn parse_image_id(value: &str) -> Result<Digest> {
    let normalized = if value.starts_with("sha256:") {
        value.to_owned()
    } else {
        format!("sha256:{value}")
    };
    Digest::parse(&normalized).map_err(|_| Error::InvalidImageDigest {
        value: value.to_owned(),
    })
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

fn peer_port_leases(paths: &Paths, hash: &WorkspaceHash) -> Result<Vec<u16>> {
    let Some(base) = paths.state_base() else {
        return Ok(Vec::new());
    };
    if let Some(entries) = wrix_state::namespace::services_at(paths.state_root())? {
        let shared_cache_port = paths
            .host_lifecycle
            .as_deref()
            .map(owner::Selected::read)
            .transpose()?
            .flatten()
            .map(|owner| owner.port.get());
        let mut ports = Vec::new();
        for entry in entries {
            let path = base.join(entry.key.as_str()).join("services.json");
            let Some(services) = read_persisted_services(&path)? else {
                continue;
            };
            if entry.key == *hash {
                continue;
            }
            let lease = PortLease::from_endpoints(&services.endpoints, &path)?;
            ports.extend(
                lease
                    .cache_http_port
                    .filter(|port| Some(*port) != shared_cache_port),
            );
            ports.extend(lease.dolt_tcp_port);
        }
        return Ok(ports);
    }
    let entries = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let shared_cache_port = paths
        .host_lifecycle
        .as_deref()
        .map(owner::Selected::read)
        .transpose()?
        .flatten()
        .map(|owner| owner.port.get());
    let mut ports = Vec::new();
    for entry in entries {
        let path = entry?.path().join("services.json");
        let Some(services) = read_persisted_services(&path)? else {
            continue;
        };
        if services.workspace_hash.matches(hash) {
            continue;
        }
        let lease = PortLease::from_endpoints(&services.endpoints, &path)?;
        ports.extend(
            lease
                .cache_http_port
                .filter(|port| Some(*port) != shared_cache_port),
        );
        ports.extend(lease.dolt_tcp_port);
    }
    Ok(ports)
}

fn select_port(
    prior: Option<u16>,
    start: u16,
    width: u16,
    hash: &WorkspaceHash,
    peer_ports: &[u16],
) -> Result<u16> {
    if let Some(port) = prior
        && (start..start + width).contains(&port)
        && !peer_ports.contains(&port)
    {
        return Ok(port);
    }
    let preferred = start + hash.port_offset(width);
    for offset in 0..width {
        let port = start + ((preferred - start + offset) % width);
        if !peer_ports.contains(&port) && is_loopback_port_available(port) {
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

fn expected_dolt_transport_label(plan: &Plan) -> &'static str {
    plan.dolt()
        .map_or(DOLT_DISABLED_LABEL_VALUE, |dolt| dolt.transport().as_str())
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
        match probe_dolt_endpoint(dolt, deadline) {
            Ok(()) => return Ok(()),
            Err(source) => {
                thread::sleep(
                    DOLT_READY_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
                );
                if Instant::now() >= deadline {
                    return Err(Error::DoltEndpointUnavailable {
                        endpoint: dolt_endpoint_description(dolt),
                        timeout: DOLT_READY_TIMEOUT.as_secs(),
                        source,
                    });
                }
            }
        }
    }
}

fn probe_dolt_endpoint(dolt: &DoltEndpoint, deadline: Instant) -> io::Result<()> {
    match dolt {
        DoltEndpoint::UnixSocket { socket_path } => connect_unix_socket(socket_path.as_path()),
        DoltEndpoint::Tcp { port, .. } => {
            let addr = SocketAddr::from(([127, 0, 0, 1], port.get()));
            TcpStream::connect_timeout(&addr, DOLT_READY_INTERVAL)?;
            dolt::probe_tcp(
                port.get(),
                deadline.saturating_duration_since(Instant::now()),
            )
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
    match dolt {
        DoltEndpoint::UnixSocket { socket_path } => socket_path.as_path().display().to_string(),
        DoltEndpoint::Tcp { port, .. } => format!("127.0.0.1:{port}"),
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
        |endpoint| match endpoint {
            DoltEndpoint::UnixSocket { .. } => format!(
                "{{ \"transport\": \"unix\", \"socket\": \"{}\", \"env\": {{ \"BEADS_DOLT_SERVER_SOCKET\": \"{}\" }} }}",
                escape_json(&endpoint.socket_path().display().to_string()),
                escape_json(&endpoint.socket_path().display().to_string())
            ),
            DoltEndpoint::Tcp { port, .. } => format!(
                "{{ \"transport\": \"tcp\", \"host\": \"127.0.0.1\", \"port\": {port}, \"env\": {{ \"BEADS_DOLT_SERVER_HOST\": \"127.0.0.1\", \"BEADS_DOLT_SERVER_PORT\": \"{port}\" }} }}"
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
        Some(DoltEndpoint::Tcp { port, .. }) => json_port(Some(port.get())),
        _ => String::from("null"),
    }
}

fn option_port(port: Option<u16>) -> String {
    port.map_or_else(|| String::from("disabled"), |value| value.to_string())
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

fn write_if_missing(path: &Path, content: impl AsRef<[u8]>) -> Result<()> {
    match wrix_state::namespace::ensure_at(path, content.as_ref())? {
        wrix_state::namespace::Source::Host(Some(_)) => (),
        wrix_state::namespace::Source::Host(None) => {
            return Err(wrix_state::store::Error::Unsupported.into());
        }
        wrix_state::namespace::Source::Observer => {
            if !path.exists() {
                fs::write(path, content)?;
            }
        }
    }
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
        is_missing_container_remove_error, parse_published_ports,
    };
    use wrix_core::path::Workspace;

    fn inspection_fixture(body: &str) -> (tempfile::TempDir, super::Runtime) {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("podman");
        std::fs::write(
            &binary,
            format!(
                "#!/usr/bin/env bash\nset -euo pipefail\n[[ \"$*\" == 'inspect --format {{{{.State.Running}}}} owner --type=container' ]] || exit 64\n{body}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runtime = super::Runtime {
            binary: binary.to_str().unwrap().to_owned(),
            kind: RuntimeKind::Podman,
            binding: None,
        };
        (root, runtime)
    }

    #[test]
    fn runtime_inspection_fixture_distinguishes_native_running_stopped_and_missing_states() {
        for (body, expected) in [
            ("printf 'true\\n'", super::RuntimeStatus::Running),
            ("printf 'false\\n'", super::RuntimeStatus::Stopped),
            (
                "printf 'Error: no such container: owner\\n' >&2; exit 125",
                super::RuntimeStatus::Missing,
            ),
        ] {
            let (_root, runtime) = inspection_fixture(body);
            assert_eq!(runtime.status_identifier("owner").unwrap(), expected);
        }
    }

    #[test]
    fn container_inspection_does_not_fall_back_to_a_same_named_image() {
        let (root, runtime) = inspection_fixture("");
        std::fs::write(
            root.path().join("podman"),
            "#!/usr/bin/env bash\nset -euo pipefail\nif [[ ${!#} == --type=container ]]; then\n  printf 'Error: no such container: owner\\n' >&2\n  exit 125\nfi\nprintf 'image metadata is not container state\\n'\n",
        )
        .unwrap();
        assert_eq!(
            runtime.status_identifier("owner").unwrap(),
            super::RuntimeStatus::Missing
        );
        assert_eq!(runtime.inspect_label("owner", "wrix.kind").unwrap(), None);
        assert_eq!(
            runtime
                .container_image_id(
                    &wrix_core::path::ContainerName::from_persisted("owner".into()).unwrap()
                )
                .unwrap(),
            None
        );
    }

    #[test]
    fn uncertain_runtime_inspection_is_not_permission_to_replace_a_backend() {
        for body in [
            "printf 'permission denied\\n' >&2; exit 125",
            "exit 1",
            "printf 'unknown\\n'",
            "printf ''",
        ] {
            let (_root, runtime) = inspection_fixture(body);
            let result = runtime.status_identifier("owner");
            assert!(
                matches!(&result, Err(super::Error::Operation { .. })),
                "uncertain inspection {body:?} returned {result:?}"
            );
        }
    }

    #[test]
    fn metadata_only_beads_plan_requires_dolt_without_creating_local_storage() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".beads")).unwrap();
        std::fs::write(
            root.path().join(".beads/metadata.json"),
            r#"{"backend":"dolt"}"#,
        )
        .unwrap();
        let workspace = Workspace::from_path(root.path()).unwrap();
        let paths = super::Paths::new(root.path().join("state"), root.path().join("cache"));
        let plan =
            Plan::for_workspace_with_paths(workspace, super::CacheMode::Disabled, paths).unwrap();
        assert!(plan.dolt().is_some());
        assert!(!root.path().join(".beads/dolt").exists());
        assert!(!root.path().join("state").exists());
    }

    #[test]
    fn service_plan_retains_the_shared_yaml_branch() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".beads")).unwrap();
        let config = root.path().join(".beads/config.yaml");
        std::fs::write(&config, "sync-branch: \"team\\u002fbeads\" # comment\n").unwrap();
        let workspace = Workspace::from_path(root.path()).unwrap();
        let paths = super::Paths::new(root.path().join("state"), root.path().join("cache"));
        let plan =
            Plan::for_workspace_with_paths(workspace, super::CacheMode::Disabled, paths).unwrap();
        assert_eq!(plan.beads_sync_branch.as_str(), "team/beads");
        let remote = root
            .path()
            .canonicalize()
            .unwrap()
            .join(".git/beads-worktrees/team/beads/.beads/dolt-remote");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::write(config, "sync-branch: [").unwrap();
        let (branch, path) = plan.beads_worktree_remote().unwrap();
        assert_eq!(branch.as_str(), "team/beads");
        assert_eq!(path, remote);
    }

    #[test]
    fn service_plan_rejects_invalid_yaml_before_layout_changes() {
        for content in [
            "sync-branch: [",
            "sync-branch: false",
            "sync-branch: /outside",
        ] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join(".beads")).unwrap();
            std::fs::write(root.path().join(".beads/config.yaml"), content).unwrap();
            let paths = super::Paths::new(root.path().join("state"), root.path().join("cache"));
            let result = Plan::for_workspace_with_paths(
                Workspace::from_path(root.path()).unwrap(),
                super::CacheMode::Disabled,
                paths,
            );
            assert!(matches!(result, Err(super::Error::BeadsConfig { .. })));
            assert!(!root.path().join("state").exists());
            assert!(!root.path().join("cache").exists());
        }
    }

    #[test]
    fn endpoint_payloads_serialize_without_missing_or_zero_tcp_ports() {
        use super::{DoltEndpoint, json_dolt_endpoint, parse_dolt_port};
        assert!(matches!(
            parse_dolt_port(0),
            Err(super::Error::ZeroDoltPort)
        ));
        let root = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_path(root.path()).unwrap();
        let socket_path = super::DoltSocketPath::for_workspace(&workspace);
        assert!(socket_path.as_path().is_absolute());
        for port in [1, 3306, u16::MAX] {
            let endpoint = DoltEndpoint::Tcp {
                socket_path: socket_path.clone(),
                port: parse_dolt_port(port).unwrap(),
            };
            let value: serde_json::Value =
                serde_json::from_str(&json_dolt_endpoint(Some(&endpoint))).unwrap();
            assert_eq!(value["transport"], "tcp");
            assert_eq!(value["host"], "127.0.0.1");
            assert_eq!(value["port"], port);
            assert_eq!(value["env"]["BEADS_DOLT_SERVER_PORT"], port.to_string());
            assert!(value.get("socket").is_none());
        }
        let endpoint = DoltEndpoint::UnixSocket { socket_path };
        let value: serde_json::Value =
            serde_json::from_str(&json_dolt_endpoint(Some(&endpoint))).unwrap();
        assert_eq!(value["transport"], "unix");
        assert!(value.get("port").is_none());
        assert_eq!(endpoint.tcp_port(), None);
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
        let root = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_current_dir().unwrap();
        let paths = super::Paths::new(root.path().join("state"), root.path().join("cache"));
        let plan = Plan::for_workspace_with_paths(
            workspace.clone(),
            super::CacheMode::Enabled,
            paths.clone(),
        )
        .unwrap();
        let second =
            Plan::for_workspace_with_paths(workspace, super::CacheMode::Enabled, paths).unwrap();
        assert_eq!(plan.cache_port(), second.cache_port());
        assert_eq!(plan.paths().state_root(), second.paths().state_root());
    }
}
