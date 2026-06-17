use std::{
    env, fs, io,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use wrix_core::path::{ContainerName, Workspace, WorkspaceHash};

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
        let workspace = Workspace::from_current_dir()?;
        Self::for_workspace(workspace, cache_mode)
    }

    pub fn for_workspace(workspace: Workspace, cache_mode: CacheMode) -> io::Result<Self> {
        let paths = Paths::for_workspace(workspace.hash())?;
        let prior_ports = PortLease::read(&paths.services_path())?;
        let cache_port = match cache_mode {
            CacheMode::Enabled => Some(select_port(
                prior_ports.cache_http_port,
                CACHE_PORT_START,
                CACHE_PORT_WIDTH,
                workspace.hash(),
            )?),
            CacheMode::Disabled => None,
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
        if self.paths.cache_secret_path().exists() && self.paths.cache_public_path().exists() {
            return Ok(());
        }
        let key_name = format!("wrix-cache-{}", self.workspace.hash());
        match generate_cache_keypair(
            &key_name,
            &self.paths.cache_secret_path(),
            &self.paths.cache_public_path(),
        ) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => write_fallback_cache_keys(
                &key_name,
                &self.paths.cache_secret_path(),
                &self.paths.cache_public_path(),
            ),
            Err(error) => Err(error),
        }
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
    let runtime = Runtime::from_env();
    runtime.ensure_running(&plan)?;
    status_for_plan(plan)
}

pub fn stop(cache_mode: CacheMode) -> io::Result<Status> {
    let plan = Plan::for_current_dir(cache_mode)?;
    let runtime = Runtime::from_env();
    runtime.remove(&plan.container_name())?;
    status_for_plan(plan)
}

pub fn status(cache_mode: CacheMode) -> io::Result<Status> {
    let plan = Plan::for_current_dir(cache_mode)?;
    status_for_plan(plan)
}

pub fn logs(cache_mode: CacheMode) -> io::Result<Vec<u8>> {
    let plan = Plan::for_current_dir(cache_mode)?;
    Runtime::from_env().logs(&plan.container_name())
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
    let runtime = Runtime::from_env().status(&plan.container_name())?;
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

struct Runtime {
    binary: String,
    image: String,
    image_source: Option<PathBuf>,
}

impl Runtime {
    fn from_env() -> Self {
        Self {
            binary: env::var("WRIX_CONTAINER_RUNTIME").unwrap_or_else(|_| default_runtime()),
            image: env::var("WRIX_SERVICE_IMAGE")
                .unwrap_or_else(|_| String::from("localhost/wrix-service:latest")),
            image_source: env::var_os("WRIX_SERVICE_IMAGE_SOURCE").map(PathBuf::from),
        }
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
            .arg(name.as_str())
            .arg("--restart=unless-stopped")
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
                    command.arg("-v").arg(format!(
                        "{}:/run/wrix:rw",
                        plan.workspace().canonical_path().join(".wrix").display()
                    ));
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
        if self.image_exists()? {
            return Ok(());
        }
        self.load_image(source)
    }

    fn image_exists(&self) -> io::Result<bool> {
        let status = if self.binary == "container" {
            Command::new(&self.binary)
                .arg("image")
                .arg("inspect")
                .arg(&self.image)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
        } else {
            Command::new(&self.binary)
                .arg("image")
                .arg("exists")
                .arg(&self.image)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
        };
        Ok(status.success())
    }

    fn load_image(&self, source: &Path) -> io::Result<()> {
        let status = if self.binary == "container" {
            Command::new(&self.binary)
                .arg("image")
                .arg("load")
                .arg("--input")
                .arg(source)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
        } else {
            Command::new(&self.binary)
                .arg("load")
                .arg("--input")
                .arg(source)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
        };
        if !status.success() {
            Err(io::Error::other(format!(
                "failed to load service image from {}",
                source.display()
            )))
        } else if self.image_exists()? || self.tag_loaded_image()? {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "loaded service image from {}, but image {} is unavailable",
                source.display(),
                self.image
            )))
        }
    }

    fn tag_loaded_image(&self) -> io::Result<bool> {
        for source in ["wrix-service:latest", "localhost/wrix-service:latest"] {
            if source == self.image {
                continue;
            }
            let status = Command::new(&self.binary)
                .arg("tag")
                .arg(source)
                .arg(&self.image)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if status.success() && self.image_exists()? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn remove(&self, name: &ContainerName) -> io::Result<()> {
        let status = Command::new(&self.binary)
            .arg("rm")
            .arg("-f")
            .arg(name.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "failed to remove service container {name}"
            )))
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
        let exists = Command::new(&self.binary)
            .arg("container")
            .arg("exists")
            .arg(name.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !exists.success() {
            return Ok(RuntimeStatus::Missing);
        }
        let output = Command::new(&self.binary)
            .arg("inspect")
            .arg("--format")
            .arg("{{.State.Running}}")
            .arg(name.as_str())
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Ok(RuntimeStatus::Stopped);
        }
        if String::from_utf8_lossy(&output.stdout).trim() == "true" {
            Ok(RuntimeStatus::Running)
        } else {
            Ok(RuntimeStatus::Stopped)
        }
    }
}

fn default_runtime() -> String {
    if cfg!(target_os = "macos") {
        String::from("container")
    } else {
        String::from("podman")
    }
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
    let base = "exec dolt sql-server --data-dir /var/lib/wrix/beads/dolt";
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

fn generate_cache_keypair(
    key_name: &str,
    secret_path: &Path,
    public_path: &Path,
) -> io::Result<()> {
    let nix_store = env::var("WRIX_NIX_STORE").unwrap_or_else(|_| String::from("nix-store"));
    let secret_tmp = secret_path.with_extension(format!("secret.{}.tmp", std::process::id()));
    let public_tmp = public_path.with_extension(format!("pub.{}.tmp", std::process::id()));
    remove_if_exists(&secret_tmp)?;
    remove_if_exists(&public_tmp)?;
    let output = Command::new(nix_store)
        .arg("--generate-binary-cache-key")
        .arg(key_name)
        .arg(&secret_tmp)
        .arg(&public_tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        remove_if_exists(&secret_tmp)?;
        remove_if_exists(&public_tmp)?;
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    fs::rename(secret_tmp, secret_path)?;
    fs::rename(public_tmp, public_path)
}

fn write_fallback_cache_keys(
    key_name: &str,
    secret_path: &Path,
    public_path: &Path,
) -> io::Result<()> {
    write_if_missing(
        secret_path,
        format!("{key_name}:missing-nix-store-secret\n"),
    )?;
    write_if_missing(
        public_path,
        format!("{key_name}:missing-nix-store-public\n"),
    )
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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
    use super::{Plan, read_endpoint_port};
    use wrix_core::path::Workspace;

    #[test]
    fn endpoint_port_reader_extracts_named_port() {
        let content = r#"{"endpoints":{"cache_http":{"host":"127.0.0.1","port":21042},"dolt_tcp":{"host":"127.0.0.1","port":23042}}}"#;
        assert_eq!(read_endpoint_port(content, "cache_http"), Some(21_042));
        assert_eq!(read_endpoint_port(content, "dolt_tcp"), Some(23_042));
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
