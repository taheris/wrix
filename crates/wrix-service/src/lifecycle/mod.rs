use std::{
    env, fs, io,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use wrix_core::{
    cache_key,
    path::{ContainerName, Workspace, WorkspaceHash},
};

const SCHEMA_VERSION: u8 = 1;
const CACHE_PORT_START: u16 = 21_000;
const CACHE_PORT_WIDTH: u16 = 2_000;
const DOLT_PORT_START: u16 = 23_000;
const DOLT_PORT_WIDTH: u16 = 2_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheMode {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug)]
pub struct Plan {
    workspace: Workspace,
    paths: Paths,
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
    pub fn for_current_dir(cache_mode: CacheMode) -> io::Result<Self> {
        let workspace = Workspace::from_service_current_dir()?;
        Self::for_workspace(workspace, cache_mode)
    }

    pub fn for_workspace(workspace: Workspace, cache_mode: CacheMode) -> io::Result<Self> {
        let paths = Paths::for_workspace(workspace.hash())?;
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
        self.workspace.container_name()
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

    fn ensure_layout(&self) -> io::Result<()> {
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

    fn write_services(&self) -> io::Result<()> {
        fs::write(self.paths.services_path(), self.services_json())
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

    fn ensure_cache_keys(&self) -> io::Result<()> {
        let key_name = format!("wrix-cache-{}", self.workspace.hash());
        let nix_store = env::var("WRIX_NIX_STORE").unwrap_or_else(|_| String::from("nix-store"));
        cache_key::ensure_keypair(
            &key_name,
            &self.paths.cache_secret_path(),
            &self.paths.cache_public_path(),
            &nix_store,
        )
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
    fn from_env() -> io::Result<Self> {
        match env::var("WRIX_DOLT_TRANSPORT") {
            Ok(value) => Self::parse(&value),
            Err(env::VarError::NotPresent) => Ok(Self::platform_default()),
            Err(env::VarError::NotUnicode(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WRIX_DOLT_TRANSPORT must be valid Unicode",
            )),
        }
    }

    fn parse(input: &str) -> io::Result<Self> {
        match input {
            "unix" | "socket" => Ok(Self::UnixSocket),
            "tcp" => Ok(Self::Tcp),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown Dolt transport: {other}"),
            )),
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
    fn for_workspace(hash: &WorkspaceHash) -> io::Result<Self> {
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

pub fn start(cache_mode: CacheMode) -> io::Result<Status> {
    let plan = Plan::for_current_dir(cache_mode)?;
    plan.ensure_layout()?;
    let runtime = Runtime::from_env()?;
    if plan.has_services() {
        runtime.ensure_running(&plan)?;
    }
    status_for_plan(plan)
}

pub fn stop(cache_mode: CacheMode) -> io::Result<Status> {
    let plan = Plan::for_current_dir(cache_mode)?;
    let runtime = Runtime::from_env()?;
    runtime.remove(&plan.container_name())?;
    status_for_plan(plan)
}

pub fn status(cache_mode: CacheMode) -> io::Result<Status> {
    let plan = Plan::for_current_dir(cache_mode)?;
    status_for_plan(plan)
}

pub fn logs(cache_mode: CacheMode) -> io::Result<Vec<u8>> {
    let plan = Plan::for_current_dir(cache_mode)?;
    Runtime::from_env()?.logs(&plan.container_name())
}

pub fn endpoints(cache_mode: CacheMode) -> io::Result<String> {
    let plan = Plan::for_current_dir(cache_mode)?;
    let path = plan.paths().services_path();
    if path.exists() {
        fs::read_to_string(path)
    } else {
        Ok(plan.services_json())
    }
}

fn status_for_plan(plan: Plan) -> io::Result<Status> {
    let runtime = Runtime::from_env()?.status(&plan.container_name())?;
    Ok(Status { runtime, plan })
}

#[derive(Clone, Copy, Debug, Default)]
struct PortLease {
    cache_http_port: Option<u16>,
    dolt_tcp_port: Option<u16>,
}

impl PortLease {
    fn read(path: &Path) -> io::Result<Self> {
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
    fn parse(input: &str) -> io::Result<Self> {
        match input {
            "nix-descriptor" => Ok(Self::NixDescriptor),
            "docker-archive" => Ok(Self::DockerArchive),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown service image source_kind: {other}"),
            )),
        }
    }

    const fn skopeo_source(self) -> &'static str {
        match self {
            Self::NixDescriptor => "nix",
            Self::DockerArchive => "docker-archive",
        }
    }
}

impl ImageSource {
    fn from_env(path: PathBuf) -> io::Result<Self> {
        let kind = match env::var("WRIX_SERVICE_IMAGE_SOURCE_KIND") {
            Ok(value) => ImageSourceKind::parse(&value)?,
            Err(env::VarError::NotPresent) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "WRIX_SERVICE_IMAGE_SOURCE_KIND is required when WRIX_SERVICE_IMAGE_SOURCE is set",
                ));
            }
            Err(env::VarError::NotUnicode(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "WRIX_SERVICE_IMAGE_SOURCE_KIND must be valid Unicode",
                ));
            }
        };
        let digest = match env::var("WRIX_SERVICE_IMAGE_DIGEST") {
            Ok(value) if value.is_empty() => None,
            Ok(value) => Some(value),
            Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "WRIX_SERVICE_IMAGE_DIGEST must be valid Unicode",
                ));
            }
        };
        Ok(Self { path, kind, digest })
    }

    fn desired_digest(&self) -> io::Result<Option<String>> {
        let Some(value) = &self.digest else {
            return Ok(None);
        };
        if value.starts_with("sha256:") {
            return Ok(Some(value.clone()));
        }
        let path = Path::new(value);
        if !path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "service image digest path does not exist: {}",
                    path.display()
                ),
            ));
        }
        let digest = fs::read_to_string(path)?.trim().to_owned();
        if digest.is_empty() {
            Ok(None)
        } else {
            Ok(Some(digest))
        }
    }
}

impl Runtime {
    fn from_env() -> io::Result<Self> {
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

    fn ensure_running(&self, plan: &Plan) -> io::Result<()> {
        let name = plan.container_name();
        match self.status(&name)? {
            RuntimeStatus::Running => return Ok(()),
            RuntimeStatus::Stopped => self.remove(&name)?,
            RuntimeStatus::Missing => {}
        }
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
            .arg("wrix.kind=service");
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
            Err(io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
    }

    fn ensure_image(&self) -> io::Result<()> {
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
            Err(io::Error::other(format!(
                "installed service image from {}, but image {} is unavailable",
                source.path.display(),
                self.image
            )))
        }
    }

    fn image_cached(&self, source: &ImageSource) -> io::Result<bool> {
        if let Some(digest) = source.desired_digest()?
            && self.image_digest_exists(&digest)?
            && (self.tag_image(&digest, &self.image)? || self.image_exists()?)
        {
            return Ok(true);
        }
        self.image_exists()
    }

    fn image_digest_exists(&self, digest: &str) -> io::Result<bool> {
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

    fn image_exists(&self) -> io::Result<bool> {
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

    fn install_image(&self, source: &ImageSource) -> io::Result<()> {
        match self.kind {
            RuntimeKind::Container => self.load_container_image(source),
            RuntimeKind::Podman => self.copy_podman_image(source),
        }
    }

    fn load_container_image(&self, source: &ImageSource) -> io::Result<()> {
        if source.kind != ImageSourceKind::DockerArchive {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "container runtime requires service image source_kind=docker-archive",
            ));
        }
        let temp_dir = create_temp_dir("wrix-service-image")?;
        let result = self.load_container_image_in_temp(source, &temp_dir);
        match (result, fs::remove_dir_all(&temp_dir)) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) | (Err(error), _) => Err(error),
        }
    }

    fn load_container_image_in_temp(
        &self,
        source: &ImageSource,
        temp_dir: &Path,
    ) -> io::Result<()> {
        let oci_archive = temp_dir.join("image.oci");
        let source_ref = format!("docker-archive:{}", source.path.display());
        let archive_ref = format!("oci-archive:{}", oci_archive.display());
        match skopeo_copy(&source_ref, &archive_ref)? {
            Ok(()) => {}
            Err(error) => return Err(io::Error::other(error)),
        }
        let output = Command::new(&self.binary)
            .arg("image")
            .arg("load")
            .arg("--input")
            .arg(&oci_archive)
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "failed to load service image from {}: {}",
                source.path.display(),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        if let Some(loaded_ref) = loaded_container_ref(&output.stdout, &output.stderr)
            && !self.tag_image(&loaded_ref, &self.image)?
        {
            return Err(io::Error::other(format!(
                "failed to tag loaded service image {loaded_ref} as {}",
                self.image
            )));
        }
        Ok(())
    }

    fn copy_podman_image(&self, source: &ImageSource) -> io::Result<()> {
        let store_ref = self.container_storage_ref()?;
        let source_ref = format!("{}:{}", source.kind.skopeo_source(), source.path.display());
        match skopeo_copy(&source_ref, &store_ref)? {
            Ok(()) => self.tag_latest(),
            Err(error)
                if source.kind == ImageSourceKind::NixDescriptor
                    && error.contains("unknown transport")
                    && error.contains("nix") =>
            {
                Self::copy_descriptor_fallback(source, &store_ref, &error)?;
                self.tag_latest()
            }
            Err(error) => Err(io::Error::other(error)),
        }
    }

    fn copy_descriptor_fallback(
        source: &ImageSource,
        store_ref: &str,
        skopeo_error: &str,
    ) -> io::Result<()> {
        let descriptor = fs::read_to_string(&source.path)?;
        let Some(stream) = json_string_field(&descriptor, "fallback_stream") else {
            return Err(io::Error::other(format!(
                "{skopeo_error}\nError: nix-descriptor source is not supported by this skopeo and has no fallback_stream: {}",
                source.path.display()
            )));
        };
        let temp_dir = create_temp_dir("wrix-service-image")?;
        let result = Self::copy_descriptor_fallback_in_temp(&stream, store_ref, &temp_dir);
        match (result, fs::remove_dir_all(&temp_dir)) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) | (Err(error), _) => Err(error),
        }
    }

    fn copy_descriptor_fallback_in_temp(
        stream: &str,
        store_ref: &str,
        temp_dir: &Path,
    ) -> io::Result<()> {
        let image_tar = temp_dir.join("image.tar");
        let output = fs::File::create(&image_tar)?;
        let status = Command::new(stream)
            .stdin(Stdio::null())
            .stdout(Stdio::from(output))
            .stderr(Stdio::inherit())
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "failed to stream service image fallback from {stream}"
            )));
        }
        let source_ref = format!("docker-archive:{}", image_tar.display());
        match skopeo_copy(&source_ref, store_ref)? {
            Ok(()) => Ok(()),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    fn container_storage_ref(&self) -> io::Result<String> {
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

    fn tag_loaded_image(&self) -> io::Result<bool> {
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

    fn tag_latest(&self) -> io::Result<()> {
        let Some(repo) = self.image.rsplit_once(':').map(|(repo, _tag)| repo) else {
            return Ok(());
        };
        let latest = format!("{repo}:latest");
        if self.tag_image(&self.image, &latest)? {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "failed to tag service image {} as {latest}",
                self.image
            )))
        }
    }

    fn tag_image(&self, source: &str, target: &str) -> io::Result<bool> {
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

    fn remove(&self, name: &ContainerName) -> io::Result<()> {
        self.remove_identifier(name.as_str()).map_err(|error| {
            io::Error::other(format!(
                "failed to remove service container {name}: {error}"
            ))
        })
    }

    fn remove_identifier(&self, identifier: &str) -> io::Result<()> {
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
                Err(io::Error::other(output_text))
            }
        }
    }

    fn logs(&self, name: &ContainerName) -> io::Result<Vec<u8>> {
        let output = Command::new(&self.binary)
            .arg("logs")
            .arg(name.as_str())
            .stdin(Stdio::null())
            .output()?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
    }

    fn status(&self, name: &ContainerName) -> io::Result<RuntimeStatus> {
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

fn skopeo_copy(source_ref: &str, store_ref: &str) -> io::Result<Result<(), String>> {
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
        || text.contains("no container with")
}

fn create_temp_dir(prefix: &str) -> io::Result<PathBuf> {
    for attempt in 0..100 {
        let path = env::temp_dir().join(format!("{prefix}-{}-{attempt}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("could not create a unique {prefix} temp directory"),
    ))
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

fn select_port(
    prior: Option<u16>,
    start: u16,
    width: u16,
    hash: &WorkspaceHash,
) -> io::Result<u16> {
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
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        format!(
            "no available loopback port in {start}-{}",
            start + width - 1
        ),
    ))
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

fn home_dir() -> io::Result<PathBuf> {
    env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "HOME is required to resolve wrix service state roots",
        )
    })
}

fn write_if_missing(path: &Path, content: impl AsRef<[u8]>) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, content)
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
        ImageSourceKind, Plan, RuntimeKind, json_string_field, loaded_container_ref,
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
    fn json_string_field_reads_descriptor_fallback_stream() {
        let content = r#"{"schema":1,"fallback_stream":"/nix/store/fake-stream"}"#;
        assert_eq!(
            json_string_field(content, "fallback_stream"),
            Some(String::from("/nix/store/fake-stream"))
        );
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
