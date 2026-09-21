mod pi_auth;

use pi_auth::Storage as PiAuth;

use std::{
    env, fmt, fs, io,
    io::Write,
    net::Ipv4Addr,
    num::NonZeroU16,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode, Output, Stdio},
};

use displaydoc::Display;
use fs2::FileExt;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;
use wrix_core::{
    cache_key::{CachePublicKey, ParseError as CachePublicKeyParseError},
    deploy_key::{Name as KeyName, ParseError as KeyNameParseError},
    path::Workspace,
};

use crate::image::{
    self, CommandStore, Digest, ImageRef, InstallRequest, RetentionRequest, Runtime, Source,
};

use super::config::{
    AgentKind, EnvName, MountMode, NetworkMode, Platform, ProfileConfig, ProfileMount,
    RuntimeSecretPolicy, Security, SpawnConfig, SpawnMount, is_known_credential_env,
};

pub struct Request {
    pub kind: Kind,
    pub profile_config_path: PathBuf,
    pub profile_config: ProfileConfig,
}

pub enum Kind {
    Run(Run),
    Spawn(Spawn),
}

pub struct Run {
    pub workspace: PathBuf,
    pub agent_args: Vec<String>,
}

pub struct Spawn {
    pub config_path: PathBuf,
    pub config: SpawnConfig,
    pub stdio: bool,
}

#[expect(
    clippy::doc_markdown,
    reason = "displaydoc comments are user-facing CLI errors and must not add Markdown backticks"
)]
#[derive(Debug, Display, Error)]
pub enum LaunchError {
    /// WRIX_NETWORK must be 'open' or 'limit' (got: {value})
    InvalidNetworkMode { value: String },
    /// project cache sandbox host must be a numeric IPv4 address: {host}
    InvalidCacheHost { host: String },
    /// project cache public key not found: {path}
    MissingCachePublicKey { path: String },
    /// project cache public key is invalid: {path}: {source}
    InvalidCachePublicKey {
        path: String,
        source: CachePublicKeyParseError,
    },
    /// project cache state root must be absolute: {path}
    InvalidCacheStateRoot { path: String },
    /// Dolt endpoint host must be a numeric IPv4 address: {host}
    InvalidDoltHost { host: String },
    /// Dolt endpoint port is invalid: {port}
    InvalidDoltPort { port: String },
    /// Dolt socket endpoint is empty
    MissingDoltSocket,
    /// service command failed: {stderr}
    ServiceFailed { stderr: String },
    /// command failed: {program}: {stderr}
    ProcessFailed { program: String, stderr: String },
    /// mount source not found: {path}
    MountSourceMissing { path: String },
    /// Unix-socket mount source rejected: {socket} -> {dest}
    SocketMountRejected { socket: String, dest: String },
    /// WRIX_DEPLOY_KEY={path}: file does not exist
    DeployKeyMissing { path: String },
    /// WRIX_SIGNING_KEY={path}: file does not exist
    SigningKeyMissing { path: String },
    /// wrix spawn: no deploy key resolved — set WRIX_DEPLOY_KEY to an existing file, or place one at {path}
    SpawnDeployKeyMissing { path: String },
    /// wrix spawn: no signing key resolved — set WRIX_SIGNING_KEY to an existing file, place one at {path}, or set WRIX_GIT_SIGN=0 to disable commit signing
    SpawnSigningKeyMissing { path: String },
    /// required runtime secret {name} is not set in the host environment or SpawnConfig.env
    RequiredRuntimeSecretMissing { name: EnvName },
    /// runtime secret {name} is not valid Unicode in the host environment
    RuntimeSecretNotUnicode { name: EnvName },
    /// runtime environment variable {name} is not valid Unicode
    RuntimeEnvironmentNotUnicode { name: &'static str },
    /// derived deploy key name is invalid: {source}
    InvalidDerivedDeployKeyName { source: KeyNameParseError },
    /// WRIX_PI_AUTH_FILE={path}: file does not exist
    PiAuthMissing { path: String },
    /// wrix spawn: Pi auth file not found at {path} — run 'pi' and /login on the host, or set WRIX_PI_AUTH_FILE to an existing auth.json
    SpawnPiAuthMissing { path: String },
    /// Pi credential storage is not an isolated regular-file store: {path}
    PiAuthStorageInvalid { path: String },
    /// stop host Pi and older containers before migrating Pi auth; a Pi lock exists at {path}.lock
    PiAuthMigrationBusy { path: String },
    /// conflicting Pi auth files at {path} and its .wrix-auth store; resolve manually without discarding credentials
    PiAuthMigrationConflict { path: String },
    /// WRIX_UNSAFE_PODMAN_SOCKET set but socket not found at {path}
    UnsafePodmanSocketMissing { path: String },
    /// path expansion requires environment variable {name}
    PathExpansionEnvMissing { name: &'static str },
    /// path expansion environment variable {name} is not valid Unicode
    PathExpansionEnvNotUnicode { name: &'static str },
    /// /dev/kvm not found. A microVM boundary requires KVM support.
    KvmMissing,
    /// krun runtime not found. A microVM boundary requires crun with libkrun.
    KrunMissing,
    /// child process returned an unsupported exit status: {code}
    InvalidExitStatus { code: i32 },
    /// {source}
    Io { source: io::Error },
    /// launch failed ({operation}) and cleanup also failed: {cleanup}
    CleanupAfterFailure {
        operation: Box<LaunchError>,
        cleanup: io::Error,
    },
    /// notification session registration count overflowed
    SessionRegistrationOverflow,
    /// invalid notification session registration JSON at {path}: {source}
    SessionRegistrationJson {
        path: String,
        source: serde_json::Error,
    },
    /// invalid service endpoint JSON: {source}
    ServiceJson { source: serde_json::Error },
    /// invalid Apple container network JSON: {source}
    DarwinNetworkJson { source: serde_json::Error },
    /// Apple container network has an unsupported IPv4 subnet: {subnet}
    DarwinNetworkSubnet { subnet: String },
    /// could not find the Apple vmnet interface for gateway {gateway}
    DarwinVmnetInterfaceMissing { gateway: String },
    /// failed to add Apple vmnet route {route} through {interface}
    DarwinVmnetRouteFailed { route: String, interface: String },
    /// {source}
    Image { source: image::Error },
}

impl From<io::Error> for LaunchError {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

impl From<image::Error> for LaunchError {
    fn from(source: image::Error) -> Self {
        Self::Image { source }
    }
}

fn complete_with_cleanup<T>(
    result: Result<T, LaunchError>,
    cleanup: Result<(), LaunchError>,
) -> Result<T, LaunchError> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_value), Err(cleanup)) => Err(cleanup),
        (Err(operation), Ok(())) => Err(operation),
        (Err(operation), Err(LaunchError::Io { source: cleanup })) => {
            Err(LaunchError::CleanupAfterFailure {
                operation: Box::new(operation),
                cleanup,
            })
        }
        (Err(operation), Err(cleanup)) => Err(LaunchError::CleanupAfterFailure {
            operation: Box::new(operation),
            cleanup: io::Error::other(cleanup.to_string()),
        }),
    }
}

pub fn execute(request: &Request, stdout: &mut impl Write) -> Result<ExitCode, LaunchError> {
    let network_mode = NetworkMode::from_env(request.profile_config.network.default_mode)?;
    let dry_run = env_flag("WRIX_DRY_RUN");
    let services = if !dry_run || env_flag("WRIX_DRY_RUN_SERVICES") {
        ServicesState::load(request)?
    } else {
        ServicesState::default()
    };
    let plan = Plan::new(request, services, network_mode)?;
    if dry_run {
        plan.write_dry_run(stdout)?;
        return Ok(ExitCode::SUCCESS);
    }
    plan.launch()
}

struct Plan<'a> {
    request: &'a Request,
    workspace: PathBuf,
    image_ref: ImageRef,
    image_source: Source,
    image_digest: Option<Digest>,
    stdio: bool,
    agent_args: Vec<String>,
    spawn_env: Vec<(String, String)>,
    runtime_secret_env: Vec<(String, String)>,
    runtime_passthrough_env: Vec<(String, String)>,
    spawn_mounts: Vec<RenderedMount>,
    services: ServicesState,
    host_podman_socket: Option<HostPodmanSocket>,
    network_mode: NetworkMode,
    git_identity: GitIdentity,
    session_id: Option<SessionId>,
}

const DARWIN_NOTIFY_TCP_ENDPOINT: &str = "192.168.64.1:5959";
const LINUX_NOTIFY_CONTAINER_DIR: &str = "/run/wrix";
const NOTIFY_SOCKET_NAME: &str = "notify.sock";
const PODMAN_SOCKET_CONTAINER_PATH: &str = "/run/podman/podman.sock";
const UNSAFE_PODMAN_SOCKET_ENV: &str = "WRIX_UNSAFE_PODMAN_SOCKET";
const LINUX_SPAWN_CONFIG_PATH: &str = "/run/wrix/spawn-config.json";
const DARWIN_SPAWN_CONFIG_DIR: &str = "/mnt/wrix/spawn-config";

#[derive(Clone, Debug)]
struct RenderedMount {
    host: String,
    container: String,
    mode: MountMode,
    optional: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DarwinBindMount {
    pub host: String,
    pub container: String,
    pub read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DarwinMountPlan {
    pub mounts: Vec<DarwinBindMount>,
    pub dir_mappings: Vec<(String, String)>,
    pub file_mappings: Vec<(String, String)>,
}

#[derive(Default)]
struct DarwinMounts {
    mounts: Vec<RenderedMount>,
    dir_mappings: Vec<(String, String)>,
    file_mappings: Vec<(String, String)>,
    file_syncs: Vec<DarwinFileSync>,
}

struct DarwinFileSync {
    source: PathBuf,
    staged: PathBuf,
}

#[derive(Default)]
struct ServicesState {
    project_cache: Option<ProjectCache>,
    beads_socket: Option<BeadsSocket>,
    beads_tcp: Option<BeadsTcp>,
}

struct ProjectCache {
    url: String,
    host: Ipv4Addr,
    port: NonZeroU16,
    nix_config: String,
}

#[derive(Deserialize)]
struct ServiceMetadata {
    state_root: PathBuf,
    endpoints: ServiceEndpoints,
}

#[derive(Deserialize)]
struct ServiceEndpoints {
    cache_http: Option<CacheHttpEndpoint>,
}

#[derive(Deserialize)]
struct CacheHttpEndpoint {
    host: Ipv4Addr,
    port: NonZeroU16,
}

fn read_cache_public_key(path: &Path) -> Result<CachePublicKey, LaunchError> {
    if !path.is_file() {
        return Err(LaunchError::MissingCachePublicKey {
            path: path.display().to_string(),
        });
    }
    CachePublicKey::parse(&fs::read_to_string(path)?).map_err(|source| {
        LaunchError::InvalidCachePublicKey {
            path: path.display().to_string(),
            source,
        }
    })
}

struct BeadsSocket {
    container_socket: String,
    mount_source: Option<PathBuf>,
}

#[derive(Deserialize)]
struct BeadsTcp {
    host: Ipv4Addr,
    port: NonZeroU16,
}

#[derive(Clone, Debug)]
struct HostPodmanSocket {
    source: PathBuf,
}

struct ImageSource {
    ref_name: ImageRef,
    source: Source,
    digest: Option<Digest>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
struct SessionId(String);

#[derive(Debug, Display, Error)]
enum SessionIdParseError {
    /// invalid tmux session identifier: {value}
    Invalid { value: String },
}

impl SessionId {
    fn parse(value: &str) -> Result<Self, SessionIdParseError> {
        let valid = value.rsplit_once(':').is_some_and(|(session, target)| {
            !session.is_empty()
                && !session.chars().any(char::is_control)
                && target.split_once('.').is_some_and(|(window, pane)| {
                    !window.is_empty()
                        && window.bytes().all(|byte| byte.is_ascii_digit())
                        && !pane.is_empty()
                        && pane.bytes().all(|byte| byte.is_ascii_digit())
                })
        });
        if !valid {
            return Err(SessionIdParseError::Invalid {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn file_name(&self) -> String {
        let safe_id = self
            .0
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                    character
                } else {
                    '-'
                }
            })
            .collect::<String>();
        format!("{safe_id}.json")
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Deserialize, Serialize)]
struct SessionRecord {
    session_id: SessionId,
    #[serde(default)]
    window_id: Option<String>,
    #[serde(default)]
    terminal_app: Option<String>,
    #[serde(default = "initial_registration_count")]
    registration_count: u64,
}

impl SessionRecord {
    fn new(session_id: &SessionId, platform: Platform, focus: Option<String>) -> Self {
        let (window_id, terminal_app) = match platform {
            Platform::Linux => (focus, None),
            Platform::Darwin => (None, focus),
        };
        Self {
            session_id: session_id.clone(),
            window_id,
            terminal_app,
            registration_count: 1,
        }
    }

    fn add_registration(
        &mut self,
        platform: Platform,
        focus: Option<String>,
    ) -> Result<(), LaunchError> {
        self.registration_count = self
            .registration_count
            .checked_add(1)
            .ok_or(LaunchError::SessionRegistrationOverflow)?;
        match (platform, focus) {
            (Platform::Linux, Some(window_id)) => self.window_id = Some(window_id),
            (Platform::Darwin, Some(terminal_app)) => self.terminal_app = Some(terminal_app),
            (_, None) => {}
        }
        Ok(())
    }
}

const fn initial_registration_count() -> u64 {
    1
}

enum SessionRegistration {
    Absent,
    Active { path: PathBuf, id: SessionId },
}

impl SessionRegistration {
    fn create(session_id: Option<&SessionId>, platform: Platform) -> Result<Self, LaunchError> {
        Self::create_in(session_id, platform, &session_directory(platform))
    }

    fn create_in(
        session_id: Option<&SessionId>,
        platform: Platform,
        directory: &Path,
    ) -> Result<Self, LaunchError> {
        let Some(session_id) = session_id else {
            return Ok(Self::Absent);
        };
        fs::create_dir_all(directory)?;
        let path = directory.join(session_id.file_name());
        let focus = focus_target(platform);
        with_session_lock(&path, || {
            let record = if path.try_exists()? {
                match read_session_record(&path) {
                    Ok(mut record) if &record.session_id == session_id => {
                        record.add_registration(platform, focus)?;
                        record
                    }
                    Ok(record) => {
                        tracing::warn!(
                            path = %path.display(),
                            registered_session_id = %record.session_id,
                            session_id = %session_id,
                            "replacing colliding notification session registration"
                        );
                        SessionRecord::new(session_id, platform, focus)
                    }
                    Err(LaunchError::SessionRegistrationJson { source, .. }) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %source,
                            "replacing malformed notification session registration"
                        );
                        SessionRecord::new(session_id, platform, focus)
                    }
                    Err(error) => return Err(error),
                }
            } else {
                SessionRecord::new(session_id, platform, focus)
            };
            write_session_record(&path, &record)
        })?;
        Ok(Self::Active {
            path,
            id: session_id.clone(),
        })
    }

    fn remove(self) -> Result<(), LaunchError> {
        let Self::Active { path, id } = self else {
            return Ok(());
        };
        with_session_lock(&path, || {
            if !path.try_exists()? {
                tracing::warn!(
                    path = %path.display(),
                    session_id = %id,
                    "notification session registration disappeared before cleanup"
                );
                return Ok(());
            }
            let mut record = read_session_record(&path)?;
            if record.session_id != id {
                tracing::warn!(
                    path = %path.display(),
                    registered_session_id = %record.session_id,
                    session_id = %id,
                    "notification session registration changed before cleanup"
                );
                return Ok(());
            }
            if record.registration_count > 1 {
                record.registration_count -= 1;
                write_session_record(&path, &record)
            } else {
                fs::remove_file(&path).map_err(LaunchError::from)
            }
        })
    }
}

fn read_session_record(path: &Path) -> Result<SessionRecord, LaunchError> {
    let content = fs::read(path)?;
    serde_json::from_slice(&content).map_err(|source| LaunchError::SessionRegistrationJson {
        path: path.display().to_string(),
        source,
    })
}

fn write_session_record(path: &Path, record: &SessionRecord) -> Result<(), LaunchError> {
    let content =
        serde_json::to_vec(record).map_err(|source| LaunchError::SessionRegistrationJson {
            path: path.display().to_string(),
            source,
        })?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&temporary, content)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn with_session_lock<T>(
    path: &Path,
    operation: impl FnOnce() -> Result<T, LaunchError>,
) -> Result<T, LaunchError> {
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))?;
    FileExt::lock_exclusive(&lock)?;
    operation()
}

struct KrunPlan {
    rows: u32,
    columns: u32,
    command: Option<String>,
}

impl KrunPlan {
    fn new(args: &[String]) -> Self {
        let (rows, columns) = terminal_size().unwrap_or((24, 80));
        let command = (!args.is_empty()).then(|| serialize_shell_args(args));
        Self {
            rows,
            columns,
            command,
        }
    }

    fn env_pairs(&self) -> Vec<(String, String)> {
        let mut pairs = vec![
            (String::from("WRIX_TERM_ROWS"), self.rows.to_string()),
            (String::from("WRIX_TERM_COLS"), self.columns.to_string()),
        ];
        if let Some(command) = &self.command {
            pairs.push((String::from("WRIX_KRUN_CMD"), command.clone()));
        }
        pairs
    }
}

impl NetworkMode {
    fn from_env(default: Self) -> Result<Self, LaunchError> {
        match env::var("WRIX_NETWORK") {
            Ok(value) => Self::parse(&value).ok_or(LaunchError::InvalidNetworkMode { value }),
            Err(env::VarError::NotPresent) => Ok(default),
            Err(env::VarError::NotUnicode(value)) => Err(LaunchError::InvalidNetworkMode {
                value: value.to_string_lossy().into_owned(),
            }),
        }
    }
}

struct GitIdentity {
    author_name: String,
    author_email: String,
    committer_name: String,
    committer_email: String,
}

impl<'a> Plan<'a> {
    fn new(
        request: &'a Request,
        services: ServicesState,
        network_mode: NetworkMode,
    ) -> Result<Self, LaunchError> {
        let profile = &request.profile_config;
        let source = match &request.kind {
            Kind::Run(_run) => ImageSource {
                ref_name: profile.image.reference.clone(),
                source: profile.image.source.clone(),
                digest: profile.image.digest.clone(),
            },
            Kind::Spawn(spawn) => {
                let config = &spawn.config;
                let ref_name = config
                    .image_ref
                    .clone()
                    .unwrap_or_else(|| profile.image.reference.clone());
                let source = config
                    .image_source
                    .as_ref()
                    .unwrap_or(&profile.image.source)
                    .clone();
                let digest =
                    if ref_name == profile.image.reference && source == profile.image.source {
                        profile.image.digest.clone()
                    } else {
                        None
                    };
                ImageSource {
                    ref_name,
                    source,
                    digest,
                }
            }
        };

        let (workspace, stdio, agent_args, spawn_env, spawn_mounts) = match &request.kind {
            Kind::Run(run) => (
                run.workspace.clone(),
                false,
                run.agent_args.clone(),
                Vec::new(),
                Vec::new(),
            ),
            Kind::Spawn(spawn) => (
                PathBuf::from(&spawn.config.workspace),
                spawn.stdio,
                spawn.config.agent_args.clone(),
                spawn
                    .config
                    .env
                    .iter()
                    .map(|(name, value)| (name.as_str().to_owned(), value.clone()))
                    .collect(),
                spawn
                    .config
                    .mounts
                    .iter()
                    .map(RenderedMount::from_spawn)
                    .collect(),
            ),
        };
        let runtime_secret_env = resolve_runtime_secret_env(&profile.security, &spawn_env)?;
        let runtime_passthrough_env = if matches!(&request.kind, Kind::Run(_)) {
            resolve_runtime_passthrough_env()?
        } else {
            Vec::new()
        };
        let host_podman_socket = if Platform::CURRENT == Platform::Linux {
            host_podman_socket_from_env()?
        } else {
            None
        };

        Ok(Self {
            request,
            workspace,
            image_ref: source.ref_name,
            image_source: source.source,
            image_digest: source.digest,
            stdio,
            agent_args,
            spawn_env,
            runtime_secret_env,
            runtime_passthrough_env,
            spawn_mounts,
            services,
            host_podman_socket,
            network_mode,
            git_identity: GitIdentity::load(),
            session_id: tmux_session_id(),
        })
    }

    fn write_dry_run(&self, stdout: &mut impl Write) -> Result<(), LaunchError> {
        Staging::with(|staging| {
            let credentials = if self.spawn() {
                self.credential_sources()?;
                None
            } else {
                self.credentials(staging)?
            };
            let pi_auth = self.pi_auth()?;

            writeln!(stdout, "SUBCOMMAND={}", self.subcommand())?;
            writeln!(stdout, "STDIO={}", u8::from(self.stdio))?;
            writeln!(
                stdout,
                "PROFILE_CONFIG={}",
                self.request.profile_config_path.display()
            )?;
            writeln!(
                stdout,
                "PROFILE_AGENT={}",
                self.request.profile_config.agent.kind.as_str()
            )?;
            writeln!(
                stdout,
                "PROFILE_NAME={}",
                self.request.profile_config.profile.name
            )?;
            writeln!(stdout, "WORKSPACE={}", self.workspace.display())?;
            writeln!(
                stdout,
                "IMAGE_OVERRIDE_REF={}",
                self.override_ref_for_dry_run()
            )?;
            writeln!(
                stdout,
                "IMAGE_OVERRIDE_SOURCE={}",
                self.override_source_for_dry_run()
            )?;
            writeln!(
                stdout,
                "IMAGE_OVERRIDE_SOURCE_KIND={}",
                self.override_source_kind_for_dry_run()
            )?;
            if Platform::CURRENT == Platform::Linux {
                writeln!(stdout, "PODMAN_NETWORK={}", linux_podman_network())?;
            }
            self.services.write_dry_run(stdout)?;
            if let Some(credentials) = &credentials {
                writeln!(stdout, "MOUNT=-v {}", credentials.mount().podman_arg())?;
                for (key, value) in credential_env_pairs(credentials) {
                    writeln!(stdout, "ENV={key}={value}")?;
                }
            }
            for (key, value) in self.launcher_identity_env_pairs() {
                writeln!(stdout, "ENV={key}={value}")?;
            }
            for (key, value) in &self.runtime_passthrough_env {
                writeln!(stdout, "ENV={key}={value}")?;
            }
            for (key, value) in &self.runtime_secret_env {
                writeln!(stdout, "ENV={key}={}", self.dry_run_env_value(key, value))?;
            }
            if let Some(path) = self.spawn_config_container_path() {
                writeln!(stdout, "ENV=WRIX_SPAWN_CONFIG={path}")?;
            }
            if let Some(auth) = &pi_auth {
                let path = pi_auth::CONTAINER_FILE;
                writeln!(stdout, "ENV=WRIX_PI_AUTH_JSON={path}")?;
                if Platform::CURRENT == Platform::Linux {
                    writeln!(stdout, "MOUNT=-v {}", auth.mount().podman_arg())?;
                }
            }
            if Platform::CURRENT == Platform::Linux {
                for mount in &self.request.profile_config.profile.mounts {
                    if let Some(rendered) = render_profile_mount(mount, staging)? {
                        writeln!(stdout, "MOUNT=-v {}", rendered.podman_arg())?;
                    }
                }
                for (key, value) in Self::linux_boundary_env_pairs() {
                    writeln!(stdout, "ENV={key}={value}")?;
                }
            }
            if let Some(socket) = &self.host_podman_socket {
                socket.write_dry_run(stdout, &self.workspace, None)?;
            }
            if let Kind::Spawn(spawn) = &self.request.kind {
                writeln!(stdout, "SPAWN_CONFIG={}", spawn.config_path.display())?;
                if Platform::CURRENT == Platform::Linux
                    && let Some(mount) = self.linux_spawn_config_mount()
                {
                    writeln!(stdout, "MOUNT=-v {}", mount.podman_arg())?;
                }
            }
            for (key, value) in &self.spawn_env {
                writeln!(stdout, "ENV={key}={}", self.dry_run_env_value(key, value))?;
            }
            for arg in &self.agent_args {
                writeln!(stdout, "CMD={arg}")?;
            }
            for mount in &self.spawn_mounts {
                writeln!(stdout, "MOUNT=-v {}", mount.podman_arg())?;
            }
            if Platform::CURRENT == Platform::Darwin {
                let mounts = self.darwin_mounts(staging, pi_auth.as_ref())?;
                mounts.write_dry_run(stdout)?;
            }
            Ok(())
        })
    }

    fn launch(&self) -> Result<ExitCode, LaunchError> {
        let registration =
            SessionRegistration::create(self.session_id.as_ref(), Platform::CURRENT)?;
        let result = match Platform::CURRENT {
            Platform::Linux => self.launch_linux(),
            Platform::Darwin => self.launch_darwin(),
        };
        complete_with_cleanup(result, registration.remove())
    }

    fn launch_linux(&self) -> Result<ExitCode, LaunchError> {
        self.install_image(Runtime::Podman)?;
        self.remember_and_prune_images(Runtime::Podman)?;
        Staging::with(|staging| {
            let mut volumes = self.linux_volumes(staging)?;
            let credentials = self.credentials(staging)?;
            if let Some(credentials) = &credentials {
                volumes.push(credentials.mount());
            }
            let pi_auth = self.pi_auth()?;
            if let Some(pi_auth) = &pi_auth {
                volumes.push(pi_auth.mount());
            }
            if let Some(spawn_mount) = self.linux_spawn_config_mount() {
                volumes.push(spawn_mount);
            }
            let staged_beads = stage_beads(&self.workspace, staging)?;
            if let Some(beads) = &staged_beads {
                volumes.push(RenderedMount {
                    host: beads.display().to_string(),
                    container: String::from("/workspace/.beads"),
                    mode: MountMode::Rw,
                    optional: false,
                });
            }
            if let Some(socket) = &self.host_podman_socket {
                volumes.push(socket.mount());
            }

            let krun = if env_flag("WRIX_MICROVM") {
                ensure_krun()?;
                Some(KrunPlan::new(&self.agent_args))
            } else {
                None
            };
            let mut command = ProcessCommand::new("podman");
            command.arg("run").arg("--rm");
            if self.spawn() {
                if self.stdio {
                    command.arg("-i");
                }
            } else {
                command.arg("-i").arg("-t");
            }
            command.arg("--cap-add=NET_ADMIN");
            if krun.is_some() {
                command
                    .arg("--runtime")
                    .arg("krun")
                    .arg("--userns=keep-id")
                    .arg("--entrypoint")
                    .arg("/krun-relay");
            }
            command
                .arg(format!(
                    "--memory={}m",
                    self.request.profile_config.resources.memory_mb
                ))
                .arg(format!(
                    "--pids-limit={}",
                    self.request.profile_config.resources.pids_limit
                ))
                .arg(format!("--network={}", linux_podman_network()))
                .arg("--mount")
                .arg("type=tmpfs,destination=/home/wrix,U=true");
            if let Some(cpus) = self.request.profile_config.resources.cpus {
                command.arg(format!("--cpus={cpus}"));
            }
            for dir in &self.request.profile_config.profile.writable_dirs {
                command
                    .arg("--mount")
                    .arg(format!("type=tmpfs,destination={dir},U=true"));
            }
            for volume in &volumes {
                command.arg("-v").arg(volume.podman_arg());
            }
            for (key, value) in self.env_pairs(
                credentials.as_ref(),
                pi_auth
                    .is_some()
                    .then_some(pi_auth::CONTAINER_FILE.to_owned()),
            ) {
                command.arg("-e").arg(format!("{key}={value}"));
            }
            for (key, value) in self.host_podman_socket_env_pairs(staged_beads.as_deref()) {
                command.arg("-e").arg(format!("{key}={value}"));
            }
            for (key, value) in Self::linux_boundary_env_pairs() {
                command.arg("-e").arg(format!("{key}={value}"));
            }
            if let Some(krun) = &krun {
                for (key, value) in krun.env_pairs() {
                    command.arg("-e").arg(format!("{key}={value}"));
                }
            }
            command
                .arg("-w")
                .arg("/workspace")
                .arg(self.image_ref.as_str());
            if krun.is_none() {
                command.args(&self.agent_args);
            }
            command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            status_to_exit(command.status()?)
        })
    }

    fn launch_darwin(&self) -> Result<ExitCode, LaunchError> {
        let vpn_conflict = fix_darwin_vmnet_route()?;
        self.install_image(Runtime::Container)?;
        self.remember_and_prune_images(Runtime::Container)?;
        Staging::with(|staging| {
            let credentials = self.credentials(staging)?;
            let pi_auth = self.pi_auth()?;
            let darwin_mounts = self.darwin_mounts(staging, pi_auth.as_ref())?;
            let mut command = ProcessCommand::new("container");
            command
                .arg("run")
                .arg("--rm")
                .arg("--cap-add")
                .arg("CAP_NET_ADMIN");
            if self.spawn() {
                if self.stdio {
                    command.arg("-i");
                }
            } else {
                command.arg("-t").arg("-i");
            }
            command
                .arg("-w")
                .arg("/")
                .arg("-c")
                .arg(self.cpus_for_darwin().to_string())
                .arg("-m")
                .arg(format!(
                    "{}M",
                    self.request.profile_config.resources.memory_mb
                ))
                .arg("--network")
                .arg("default")
                .arg("-v")
                .arg(format!("{}:/workspace", self.workspace.display()));
            for mount in &darwin_mounts.mounts {
                command.arg("-v").arg(mount.podman_arg());
            }
            if let Some(credentials) = &credentials {
                command.arg("-v").arg(credentials.mount().podman_arg());
            }
            for (key, value) in self.env_pairs(
                credentials.as_ref(),
                pi_auth
                    .is_some()
                    .then_some(pi_auth::CONTAINER_FILE.to_owned()),
            ) {
                command.arg("-e").arg(format!("{key}={value}"));
            }
            command
                .arg("-e")
                .arg(format!("HOST_UID={}", current_uid()?));
            if let Some(value) = darwin_mounts.dir_env() {
                command.arg("-e").arg(format!("WRIX_DIR_MOUNTS={value}"));
            }
            if let Some(value) = darwin_mounts.file_env() {
                command.arg("-e").arg(format!("WRIX_FILE_MOUNTS={value}"));
            }
            if vpn_conflict {
                command.arg("-e").arg("WRIX_WAIT_FOR_ROUTE=1");
            }
            command
                .arg("--")
                .arg(self.image_ref.as_str())
                .args(&self.agent_args)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            let status = command.status()?;
            darwin_mounts.sync_files()?;
            status_to_exit(status)
        })
    }

    fn linux_volumes(&self, staging: &Staging) -> Result<Vec<RenderedMount>, LaunchError> {
        let mut volumes = vec![RenderedMount {
            host: self.workspace.display().to_string(),
            container: String::from("/workspace"),
            mode: MountMode::Rw,
            optional: false,
        }];
        if let Some(notification) = linux_notification_socket_mount()? {
            volumes.push(notification);
        }
        if let Some(beads) = &self.services.beads_socket
            && let Some(source) = &beads.mount_source
        {
            volumes.push(RenderedMount {
                host: source.display().to_string(),
                container: String::from("/run/wrix/dolt"),
                mode: MountMode::Rw,
                optional: false,
            });
        }
        for mount in &self.request.profile_config.profile.mounts {
            if let Some(rendered) = render_profile_mount(mount, staging)? {
                volumes.push(rendered);
            }
        }
        volumes.extend(self.spawn_mounts.clone());
        Ok(volumes)
    }

    fn darwin_mounts(
        &self,
        staging: &Staging,
        pi_auth: Option<&PiAuth>,
    ) -> Result<DarwinMounts, LaunchError> {
        let mut mounts = darwin_mounts_from_rendered(
            &self.request.profile_config.profile.mounts,
            &self.spawn_mounts,
            &staging.root,
        )?;
        if let Some(spawn_mount) = self.stage_darwin_spawn_config(staging)? {
            mounts.mounts.push(spawn_mount);
        }
        if let Some(auth) = pi_auth {
            mounts.mounts.push(auth.mount());
        }
        Ok(mounts)
    }

    fn linux_spawn_config_mount(&self) -> Option<RenderedMount> {
        let Kind::Spawn(spawn) = &self.request.kind else {
            return None;
        };
        Some(RenderedMount {
            host: spawn.config_path.display().to_string(),
            container: String::from(LINUX_SPAWN_CONFIG_PATH),
            mode: MountMode::Ro,
            optional: false,
        })
    }

    fn stage_darwin_spawn_config(
        &self,
        staging: &Staging,
    ) -> Result<Option<RenderedMount>, LaunchError> {
        let Kind::Spawn(spawn) = &self.request.kind else {
            return Ok(None);
        };
        let host = staging.root.join("spawn-config");
        fs::create_dir_all(&host)?;
        fs::copy(&spawn.config_path, host.join("spawn-config.json"))?;
        Ok(Some(RenderedMount {
            host: host.display().to_string(),
            container: String::from(DARWIN_SPAWN_CONFIG_DIR),
            mode: MountMode::Ro,
            optional: false,
        }))
    }

    fn spawn_config_container_path(&self) -> Option<&'static str> {
        self.spawn().then_some(match Platform::CURRENT {
            Platform::Linux => LINUX_SPAWN_CONFIG_PATH,
            Platform::Darwin => "/mnt/wrix/spawn-config/spawn-config.json",
        })
    }

    fn install_image(&self, runtime: Runtime) -> Result<(), LaunchError> {
        let mut store = CommandStore;
        image::install(
            &mut store,
            &InstallRequest::new(
                runtime,
                &self.image_ref,
                &self.image_source,
                self.image_digest.as_ref(),
            )?,
        )?;
        Ok(())
    }

    fn remember_and_prune_images(&self, runtime: Runtime) -> Result<(), LaunchError> {
        let mru_path = image::default_mru_path();
        let mut store = CommandStore;
        image::remember_and_prune(
            &mut store,
            &RetentionRequest {
                runtime,
                image_ref: &self.image_ref,
                source: Some(&self.image_source),
                digest: self.image_digest.as_ref(),
                mru_path: &mru_path,
            },
        )?;
        Ok(())
    }

    fn credential_sources(&self) -> Result<Option<CredentialSources>, LaunchError> {
        let name = deploy_key_name(
            &self.workspace,
            self.request.profile_config.security.deploy_key.as_ref(),
        )?;
        let deploy = resolve_key("WRIX_DEPLOY_KEY", name.as_str(), false)?;
        let signing_name = format!("{name}-signing");
        let signing = resolve_key("WRIX_SIGNING_KEY", &signing_name, true)?;
        if self.spawn() {
            if deploy.is_none() {
                return Err(LaunchError::SpawnDeployKeyMissing {
                    path: default_key_path(name.as_str()).display().to_string(),
                });
            }
            if env::var("WRIX_GIT_SIGN").unwrap_or_else(|_| String::from("1")) != "0"
                && signing.is_none()
            {
                return Err(LaunchError::SpawnSigningKeyMissing {
                    path: default_key_path(&signing_name).display().to_string(),
                });
            }
        }
        let Some(deploy) = deploy else {
            return Ok(None);
        };
        Ok(Some(CredentialSources {
            deploy,
            signing,
            name,
        }))
    }

    fn credentials(&self, staging: &Staging) -> Result<Option<Credentials>, LaunchError> {
        let Some(sources) = self.credential_sources()? else {
            return Ok(None);
        };
        let key_root = staging.root.join("deploy_keys");
        fs::create_dir_all(&key_root)?;
        let deploy_target = key_root.join(sources.name.as_str());
        fs::copy(&sources.deploy, &deploy_target)?;
        let signing_target = if let Some(path) = sources.signing {
            let target = key_root.join(format!("{}-signing", sources.name));
            fs::copy(path, &target)?;
            Some(target)
        } else {
            None
        };
        Ok(Some(Credentials {
            deploy: deploy_target,
            signing: signing_target,
            name: sources.name,
        }))
    }

    fn pi_auth(&self) -> Result<Option<PiAuth>, LaunchError> {
        if self.request.profile_config.agent.kind != AgentKind::Pi {
            return Ok(None);
        }
        let path = env::var_os("WRIX_PI_AUTH_FILE")
            .map_or_else(|| home_dir().join(".pi/agent/auth.json"), PathBuf::from);
        let allow_create = !self.spawn() && env::var_os("WRIX_PI_AUTH_FILE").is_none();
        match PiAuth::prepare(&path, allow_create) {
            Err(LaunchError::PiAuthMissing { path }) if self.spawn() => {
                Err(LaunchError::SpawnPiAuthMissing { path })
            }
            result => result.map(Some),
        }
    }

    fn host_podman_socket_env_pairs(&self, staged_beads: Option<&Path>) -> Vec<(String, String)> {
        self.host_podman_socket.as_ref().map_or_else(Vec::new, |_| {
            HostPodmanSocket::env_pairs(&self.workspace, staged_beads)
        })
    }

    fn env_pairs(
        &self,
        credentials: Option<&Credentials>,
        pi_auth: Option<String>,
    ) -> Vec<(String, String)> {
        let mut pairs = self
            .request
            .profile_config
            .profile
            .env
            .iter()
            .map(|(key, value)| (key.as_str().to_owned(), value.clone()))
            .collect::<Vec<_>>();
        pairs.extend(self.runtime_passthrough_env.iter().cloned());
        pairs.extend(self.runtime_secret_env.iter().cloned());
        if self.spawn() {
            pairs.extend(self.spawn_env.iter().cloned());
            if self.stdio {
                pairs.push((String::from("WRIX_STDIO"), String::from("1")));
            }
        }
        if let Some(path) = self.spawn_config_container_path() {
            pairs.push((String::from("WRIX_SPAWN_CONFIG"), path.to_owned()));
        }
        pairs.extend([
            (String::from("BD_NO_DAEMON"), String::from("1")),
            (String::from("HOME"), String::from("/home/wrix")),
            (
                String::from("GIT_AUTHOR_NAME"),
                self.git_identity.author_name.clone(),
            ),
            (
                String::from("GIT_AUTHOR_EMAIL"),
                self.git_identity.author_email.clone(),
            ),
            (
                String::from("GIT_COMMITTER_NAME"),
                self.git_identity.committer_name.clone(),
            ),
            (
                String::from("GIT_COMMITTER_EMAIL"),
                self.git_identity.committer_email.clone(),
            ),
        ]);
        pairs.extend(self.launcher_identity_env_pairs());
        if let Some(pair) = notification_env(Platform::CURRENT) {
            pairs.push(pair);
        }
        if let Ok(value) = env::var("WRIX_GIT_SIGN") {
            pairs.push((String::from("WRIX_GIT_SIGN"), value));
        }
        if let Some(session_id) = &self.session_id {
            pairs.push((
                String::from("WRIX_SESSION_ID"),
                session_id.as_str().to_owned(),
            ));
        }
        if let Some(cache) = &self.services.project_cache {
            pairs.push((
                String::from("WRIX_PROJECT_CACHE_HOST"),
                cache.host.to_string(),
            ));
            pairs.push((
                String::from("WRIX_PROJECT_CACHE_PORT"),
                cache.port.to_string(),
            ));
            pairs.push((String::from("NIX_CONFIG"), cache.nix_config.clone()));
        }
        if let Some(beads) = &self.services.beads_socket {
            pairs.push((
                String::from("BEADS_DOLT_SERVER_SOCKET"),
                beads.container_socket.clone(),
            ));
        }
        if let Some(beads) = &self.services.beads_tcp {
            pairs.push((
                String::from("BEADS_DOLT_SERVER_HOST"),
                beads.host.to_string(),
            ));
            pairs.push((
                String::from("BEADS_DOLT_SERVER_PORT"),
                beads.port.to_string(),
            ));
        }
        if let Some(auth) = pi_auth {
            pairs.push((String::from("WRIX_PI_AUTH_JSON"), auth));
        }
        if let Some(credentials) = credentials {
            pairs.extend(credential_env_pairs(credentials));
        }
        pairs
    }

    fn dry_run_env_value<'b>(&self, name: &str, value: &'b str) -> &'b str {
        if is_known_credential_env(name)
            || self
                .request
                .profile_config
                .security
                .runtime_secrets
                .keys()
                .any(|secret| secret.as_str() == name)
        {
            "[REDACTED]"
        } else {
            value
        }
    }

    fn launcher_identity_env_pairs(&self) -> Vec<(String, String)> {
        vec![
            (
                String::from("WRIX_AGENT"),
                self.request.profile_config.agent.kind.as_str().to_owned(),
            ),
            (
                String::from("WRIX_NETWORK"),
                self.network_mode.as_str().to_owned(),
            ),
            (
                String::from("WRIX_NETWORK_ALLOWLIST"),
                self.request
                    .profile_config
                    .profile
                    .network_allowlist
                    .join(","),
            ),
        ]
    }

    fn linux_boundary_env_pairs() -> Vec<(String, String)> {
        if env_flag("WRIX_MICROVM") {
            Vec::new()
        } else {
            vec![(String::from("IS_SANDBOX"), String::from("1"))]
        }
    }

    const fn subcommand(&self) -> &'static str {
        match self.request.kind {
            Kind::Run(_) => "run",
            Kind::Spawn(_) => "spawn",
        }
    }

    const fn spawn(&self) -> bool {
        matches!(self.request.kind, Kind::Spawn(_))
    }

    fn override_ref_for_dry_run(&self) -> &str {
        match &self.request.kind {
            Kind::Spawn(spawn) => spawn.config.image_ref.as_ref().map_or("", ImageRef::as_str),
            Kind::Run(_) => "",
        }
    }

    fn override_source_for_dry_run(&self) -> std::borrow::Cow<'_, str> {
        match &self.request.kind {
            Kind::Spawn(spawn) => spawn
                .config
                .image_source
                .as_ref()
                .map_or_else(|| "".into(), |source| source.path().to_string_lossy()),
            Kind::Run(_) => "".into(),
        }
    }

    fn override_source_kind_for_dry_run(&self) -> &str {
        match &self.request.kind {
            Kind::Spawn(spawn) => spawn
                .config
                .image_source
                .as_ref()
                .map_or("", |source| source.kind().as_str()),
            Kind::Run(_) => "",
        }
    }

    fn cpus_for_darwin(&self) -> u32 {
        self.request.profile_config.resources.cpus.unwrap_or(2)
    }
}

impl HostPodmanSocket {
    fn mount(&self) -> RenderedMount {
        RenderedMount {
            host: self.source.display().to_string(),
            container: String::from(PODMAN_SOCKET_CONTAINER_PATH),
            mode: MountMode::Rw,
            optional: false,
        }
    }

    fn env_pairs(workspace: &Path, staged_beads: Option<&Path>) -> Vec<(String, String)> {
        let mut pairs = vec![
            (
                String::from("CONTAINER_HOST"),
                format!("unix://{PODMAN_SOCKET_CONTAINER_PATH}"),
            ),
            (
                String::from("GC_HOST_WORKSPACE"),
                workspace.display().to_string(),
            ),
        ];
        if let Some(beads) = staged_beads {
            pairs.push((String::from("GC_HOST_BEADS"), beads.display().to_string()));
        }
        pairs
    }

    fn write_dry_run(
        &self,
        stdout: &mut impl Write,
        workspace: &Path,
        staged_beads: Option<&Path>,
    ) -> Result<(), LaunchError> {
        writeln!(stdout, "MOUNT=-v {}", self.mount().podman_arg())?;
        for (key, value) in Self::env_pairs(workspace, staged_beads) {
            writeln!(stdout, "ENV={key}={value}")?;
        }
        Ok(())
    }
}

impl GitIdentity {
    fn load() -> Self {
        let author_name = first_configured_value(&[
            env_value("GIT_AUTHOR_NAME"),
            env_value("GIT_COMMITTER_NAME"),
            git_global_config("user.name"),
        ])
        .unwrap_or_else(|| String::from("Wrix Sandbox"));
        let author_email = first_configured_value(&[
            env_value("GIT_AUTHOR_EMAIL"),
            env_value("GIT_COMMITTER_EMAIL"),
            git_global_config("user.email"),
        ])
        .unwrap_or_else(|| String::from("sandbox@wrix.dev"));
        let committer_name = env_value("GIT_COMMITTER_NAME").unwrap_or_else(|| author_name.clone());
        let committer_email =
            env_value("GIT_COMMITTER_EMAIL").unwrap_or_else(|| author_email.clone());
        Self {
            author_name,
            author_email,
            committer_name,
            committer_email,
        }
    }
}

impl RenderedMount {
    fn from_profile(mount: &ProfileMount) -> Self {
        Self {
            host: mount.source.clone(),
            container: mount.dest.clone(),
            mode: mount.mode,
            optional: mount.optional,
        }
    }

    fn from_spawn(mount: &SpawnMount) -> Self {
        Self {
            host: mount.host_path.clone(),
            container: mount.container_path.clone(),
            mode: if mount.read_only {
                MountMode::Ro
            } else {
                MountMode::Rw
            },
            optional: false,
        }
    }

    fn podman_arg(&self) -> String {
        if self.mode == MountMode::Ro {
            format!("{}:{}:ro", self.host, self.container)
        } else {
            format!("{}:{}", self.host, self.container)
        }
    }
}

impl DarwinMountPlan {
    pub fn dir_env(&self) -> Option<String> {
        mapping_env(&self.dir_mappings, |source| {
            self.mapping_is_read_only(source)
        })
    }

    pub fn file_env(&self) -> Option<String> {
        mapping_env(&self.file_mappings, |source| {
            self.mapping_is_read_only(source)
        })
    }

    fn mapping_is_read_only(&self, source: &str) -> bool {
        self.mounts
            .iter()
            .find(|mount| mapping_uses_mount(source, &mount.container))
            .is_none_or(|mount| mount.read_only)
    }
}

impl From<DarwinMounts> for DarwinMountPlan {
    fn from(mounts: DarwinMounts) -> Self {
        Self {
            mounts: mounts
                .mounts
                .into_iter()
                .map(|mount| DarwinBindMount {
                    host: mount.host,
                    container: mount.container,
                    read_only: mount.mode == MountMode::Ro,
                })
                .collect(),
            dir_mappings: mounts.dir_mappings,
            file_mappings: mounts.file_mappings,
        }
    }
}

pub fn classify_darwin_mounts(
    profile_mounts: &[ProfileMount],
    spawn_mounts: &[SpawnMount],
    staging_root: &Path,
) -> Result<DarwinMountPlan, LaunchError> {
    let rendered_spawn = spawn_mounts
        .iter()
        .map(RenderedMount::from_spawn)
        .collect::<Vec<_>>();
    darwin_mounts_from_rendered(profile_mounts, &rendered_spawn, staging_root)
        .map(DarwinMountPlan::from)
}

fn darwin_mounts_from_rendered(
    profile_mounts: &[ProfileMount],
    spawn_mounts: &[RenderedMount],
    staging_root: &Path,
) -> Result<DarwinMounts, LaunchError> {
    let mut mounts = DarwinMounts::default();
    for mount in profile_mounts {
        mounts.push(&RenderedMount::from_profile(mount), staging_root)?;
    }
    for mount in spawn_mounts {
        mounts.push(mount, staging_root)?;
    }
    Ok(mounts)
}

impl DarwinMounts {
    fn push(&mut self, mount: &RenderedMount, staging_root: &Path) -> Result<(), LaunchError> {
        let source = expand_path(&mount.host)?;
        if !source.exists() {
            if mount.optional {
                return Ok(());
            }
            return Err(LaunchError::MountSourceMissing {
                path: source.display().to_string(),
            });
        }
        if file_type_is_socket(&source)? {
            return Err(LaunchError::SocketMountRejected {
                socket: source.display().to_string(),
                dest: mount.container.clone(),
            });
        }
        if source.is_dir() {
            let index = self.dir_mappings.len();
            let host = staging_root.join(format!("dir{index}"));
            copy_dir(&source, &host)?;
            let container = format!("/mnt/wrix/dir{index}");
            self.mounts.push(RenderedMount {
                host: host.display().to_string(),
                container: container.clone(),
                mode: mount.mode,
                optional: false,
            });
            self.dir_mappings.push((container, mount.container.clone()));
            return Ok(());
        }
        let index = self.file_mappings.len();
        let file_name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let host = staging_root.join(format!("file{index}"));
        fs::create_dir_all(&host)?;
        let staged = host.join(&file_name);
        fs::copy(&source, &staged)?;
        let container = format!("/mnt/wrix/file{index}");
        self.mounts.push(RenderedMount {
            host: host.display().to_string(),
            container: container.clone(),
            mode: mount.mode,
            optional: false,
        });
        self.file_mappings
            .push((format!("{container}/{file_name}"), mount.container.clone()));
        if mount.mode == MountMode::Rw {
            self.file_syncs.push(DarwinFileSync { source, staged });
        }
        Ok(())
    }

    fn dir_env(&self) -> Option<String> {
        mapping_env(&self.dir_mappings, |source| {
            self.mapping_is_read_only(source)
        })
    }

    fn file_env(&self) -> Option<String> {
        mapping_env(&self.file_mappings, |source| {
            self.mapping_is_read_only(source)
        })
    }

    fn mapping_is_read_only(&self, source: &str) -> bool {
        self.mounts
            .iter()
            .find(|mount| mapping_uses_mount(source, &mount.container))
            .is_none_or(|mount| mount.mode == MountMode::Ro)
    }

    fn sync_files(&self) -> Result<(), LaunchError> {
        for sync in &self.file_syncs {
            fs::copy(&sync.staged, &sync.source)?;
        }
        Ok(())
    }

    fn write_dry_run(&self, stdout: &mut impl Write) -> Result<(), LaunchError> {
        if let Some(value) = self.dir_env() {
            writeln!(stdout, "DIR_MOUNTS={value}")?;
        }
        if let Some(value) = self.file_env() {
            writeln!(stdout, "FILE_MOUNTS={value}")?;
        }
        for mount in &self.mounts {
            writeln!(stdout, "MOUNT=-v {}", mount.podman_arg())?;
        }
        Ok(())
    }
}

impl ServicesState {
    fn load(request: &Request) -> Result<Self, LaunchError> {
        let workspace = match &request.kind {
            Kind::Run(run) => run.workspace.as_path(),
            Kind::Spawn(spawn) => Path::new(&spawn.config.workspace),
        };
        let mut state = Self::default();
        let dolt = service_workspace_has_dolt(workspace)?;
        if request.profile_config.services.nix_cache.enabled {
            run_service(workspace, &["service", "start"])?;
            state.load_project_cache(workspace)?;
        } else if dolt {
            run_service(workspace, &["service", "start", "--no-cache"])?;
        }
        if dolt {
            run_service(workspace, &["service", "dolt", "wait"])?;
            state.load_dolt(workspace)?;
        }
        Ok(state)
    }

    fn load_project_cache(&mut self, workspace: &Path) -> Result<(), LaunchError> {
        let output = run_service(workspace, &["service", "endpoints"])?;
        let metadata = serde_json::from_slice::<ServiceMetadata>(&output.stdout)
            .map_err(|source| LaunchError::ServiceJson { source })?;
        let Some(endpoint) = metadata.endpoints.cache_http else {
            return Ok(());
        };
        if !metadata.state_root.is_absolute() {
            return Err(LaunchError::InvalidCacheStateRoot {
                path: metadata.state_root.display().to_string(),
            });
        }
        let public_key_path = metadata.state_root.join("keys/cache.pub");
        let public_key = read_cache_public_key(&public_key_path)?;
        let sandbox_host = sandbox_cache_host(endpoint.host)?;
        let url = format!("http://{sandbox_host}:{}", endpoint.port);
        let nix_config = format!(
            "extra-substituters = {url}\nextra-trusted-public-keys = {}\nbuilders-use-substitutes = true",
            public_key.as_str()
        );
        self.project_cache = Some(ProjectCache {
            url,
            host: sandbox_host,
            port: endpoint.port,
            nix_config,
        });
        Ok(())
    }

    fn load_dolt(&mut self, workspace: &Path) -> Result<(), LaunchError> {
        if Platform::CURRENT == Platform::Linux {
            let output = run_service(workspace, &["service", "dolt", "socket"])?;
            let socket = trim_stdout(&output.stdout);
            self.beads_socket = Some(configure_dolt_socket(workspace, &socket)?);
        } else {
            let output = run_service(workspace, &["service", "dolt", "sandbox-endpoint"])?;
            let endpoint: BeadsTcp = serde_json::from_slice(&output.stdout)
                .map_err(|source| LaunchError::ServiceJson { source })?;
            if endpoint.host.is_loopback() || endpoint.host.is_unspecified() {
                return Err(LaunchError::InvalidDoltHost {
                    host: endpoint.host.to_string(),
                });
            }
            self.beads_tcp = Some(endpoint);
        }
        Ok(())
    }

    fn write_dry_run(&self, stdout: &mut impl Write) -> Result<(), LaunchError> {
        if let Some(cache) = &self.project_cache {
            writeln!(stdout, "PROJECT_CACHE_URL={}", cache.url)?;
            writeln!(stdout, "ENV=WRIX_PROJECT_CACHE_HOST={}", cache.host)?;
            writeln!(stdout, "ENV=WRIX_PROJECT_CACHE_PORT={}", cache.port)?;
            writeln!(stdout, "ENV=NIX_CONFIG={}", cache.nix_config)?;
        }
        if let Some(beads) = &self.beads_socket {
            writeln!(
                stdout,
                "ENV=BEADS_DOLT_SERVER_SOCKET={}",
                beads.container_socket
            )?;
            if let Some(source) = &beads.mount_source {
                writeln!(stdout, "MOUNT=-v {}:/run/wrix/dolt:rw", source.display())?;
            }
        }
        if let Some(beads) = &self.beads_tcp {
            writeln!(stdout, "ENV=BEADS_DOLT_SERVER_PORT={}", beads.port)?;
            writeln!(stdout, "ENV=BEADS_DOLT_SERVER_HOST={}", beads.host)?;
        }
        Ok(())
    }
}

struct CredentialSources {
    deploy: PathBuf,
    signing: Option<PathBuf>,
    name: KeyName,
}

struct Credentials {
    deploy: PathBuf,
    signing: Option<PathBuf>,
    name: KeyName,
}

impl Credentials {
    fn mount(&self) -> RenderedMount {
        let host = self
            .deploy
            .parent()
            .map_or_else(|| self.deploy.clone(), Path::to_path_buf);
        RenderedMount {
            host: host.display().to_string(),
            container: String::from("/etc/wrix/keys"),
            mode: MountMode::Ro,
            optional: false,
        }
    }
}

fn credential_env_pairs(credentials: &Credentials) -> Vec<(String, String)> {
    let mut pairs = vec![(
        String::from("WRIX_DEPLOY_KEY"),
        format!("/etc/wrix/keys/{}", credentials.name),
    )];
    if credentials.signing.is_some() {
        pairs.push((
            String::from("WRIX_SIGNING_KEY"),
            format!("/etc/wrix/keys/{}-signing", credentials.name),
        ));
    }
    pairs
}

struct Staging {
    root: PathBuf,
}

impl Staging {
    fn create() -> Result<Self, LaunchError> {
        let root = env::var_os("XDG_CACHE_HOME")
            .map_or_else(|| home_dir().join(".cache"), PathBuf::from)
            .join("wrix/mounts")
            .join(std::process::id().to_string());
        if root.exists() {
            fs::remove_dir_all(&root)?;
        }
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn with<T>(operation: impl FnOnce(&Self) -> Result<T, LaunchError>) -> Result<T, LaunchError> {
        let staging = Self::create()?;
        let result = operation(&staging);
        let cleanup = fs::remove_dir_all(&staging.root).map_err(LaunchError::from);
        complete_with_cleanup(result, cleanup)
    }
}

fn render_profile_mount(
    mount: &ProfileMount,
    staging: &Staging,
) -> Result<Option<RenderedMount>, LaunchError> {
    let source = expand_path(&mount.source)?;
    if !source.exists() {
        if mount.optional {
            return Ok(None);
        }
        return Err(LaunchError::MountSourceMissing {
            path: source.display().to_string(),
        });
    }
    if source.is_dir() && mount.mode == MountMode::Ro {
        let target = staging
            .root
            .join(format!("dir{}", stable_mount_index(&source)));
        copy_dir(&source, &target)?;
        return Ok(Some(RenderedMount {
            host: target.display().to_string(),
            container: mount.dest.clone(),
            mode: mount.mode,
            optional: false,
        }));
    }
    if file_type_is_socket(&source)? {
        return Err(LaunchError::SocketMountRejected {
            socket: source.display().to_string(),
            dest: mount.dest.clone(),
        });
    }
    Ok(Some(RenderedMount {
        host: source.display().to_string(),
        container: mount.dest.clone(),
        mode: mount.mode,
        optional: false,
    }))
}

fn stage_beads(workspace: &Path, staging: &Staging) -> Result<Option<PathBuf>, LaunchError> {
    let beads = workspace.join(".beads");
    if !beads.is_dir() {
        return Ok(None);
    }
    let target = staging.root.join("beads");
    fs::create_dir_all(&target)?;
    copy_if_exists(&beads.join("config.yaml"), &target.join("config.yaml"))?;
    copy_if_exists(&beads.join("metadata.json"), &target.join("metadata.json"))?;
    Ok(Some(target))
}

fn copy_if_exists(source: &Path, target: &Path) -> Result<(), LaunchError> {
    if source.is_file() {
        fs::copy(source, target)?;
    }
    Ok(())
}

fn copy_dir(source: &Path, target: &Path) -> Result<(), LaunchError> {
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target_path = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target_path)?;
        } else {
            fs::copy(entry.path(), target_path)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn file_type_is_socket(path: &Path) -> Result<bool, LaunchError> {
    use std::os::unix::fs::FileTypeExt;

    Ok(fs::metadata(path)?.file_type().is_socket())
}

#[cfg(not(unix))]
fn file_type_is_socket(_path: &Path) -> Result<bool, LaunchError> {
    Ok(false)
}

fn service_workspace_has_dolt(workspace: &Path) -> Result<bool, LaunchError> {
    let identity = Workspace::from_service_path(workspace)?;
    Ok(identity.canonical_path().join(".beads/dolt").is_dir())
}

fn configure_dolt_socket(workspace: &Path, socket: &str) -> Result<BeadsSocket, LaunchError> {
    if socket.is_empty() {
        return Err(LaunchError::MissingDoltSocket);
    }
    let project_real = workspace.canonicalize()?;
    let workspace_socket = project_real.join(".wrix/dolt.sock");
    if Path::new(socket) == workspace_socket {
        return Ok(BeadsSocket {
            container_socket: String::from("/workspace/.wrix/dolt.sock"),
            mount_source: None,
        });
    }
    Ok(BeadsSocket {
        container_socket: String::from("/run/wrix/dolt/dolt.sock"),
        mount_source: Path::new(socket).parent().map(Path::to_path_buf),
    })
}

fn sandbox_cache_host(host: Ipv4Addr) -> Result<Ipv4Addr, LaunchError> {
    match env::var("WRIX_PROJECT_CACHE_SANDBOX_HOST") {
        Ok(value) => value
            .parse::<Ipv4Addr>()
            .map_err(|_source| LaunchError::InvalidCacheHost { host: value }),
        Err(env::VarError::NotPresent)
            if Platform::CURRENT == Platform::Linux && host.is_loopback() =>
        {
            Ok(Ipv4Addr::new(169, 254, 1, 2))
        }
        Err(env::VarError::NotPresent) => Ok(host),
        Err(env::VarError::NotUnicode(value)) => Err(LaunchError::InvalidCacheHost {
            host: value.to_string_lossy().into_owned(),
        }),
    }
}

fn run_service(workspace: &Path, args: &[&str]) -> Result<Output, LaunchError> {
    let output = run_service_output(workspace, args)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(LaunchError::ServiceFailed {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn run_service_output(workspace: &Path, args: &[&str]) -> Result<Output, LaunchError> {
    let current = service_command()?;
    let mut command = ProcessCommand::new(current);
    command
        .args(args)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .output()
        .map_err(LaunchError::from)
}

fn service_command() -> Result<PathBuf, LaunchError> {
    if env_flag("WRIX_DRY_RUN")
        && let Some(path) = env::var_os("WRIX_DRY_RUN_SERVICE_BIN")
    {
        return Ok(PathBuf::from(path));
    }
    env::current_exe().map_err(LaunchError::from)
}

fn run_required_output(program: &str, args: &[&str]) -> Result<Output, LaunchError> {
    let output = run_output(program, args)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(LaunchError::ProcessFailed {
            program: program.to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn run_output(program: &str, args: &[&str]) -> Result<Output, LaunchError> {
    ProcessCommand::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(LaunchError::from)
}

#[derive(Debug, Eq, PartialEq)]
struct DarwinNetwork {
    subnet: String,
    gateway: String,
}

#[derive(Debug, Eq, PartialEq)]
struct DarwinSplitRoute {
    network: String,
    probe: String,
}

fn fix_darwin_vmnet_route() -> Result<bool, LaunchError> {
    let default_route = run_required_output("route", &["-n", "get", "default"])?;
    let Some(default_interface) = route_interface(&default_route.stdout) else {
        return Ok(false);
    };
    if !default_interface.starts_with("utun") {
        return Ok(false);
    }

    let network_output = run_required_output("container", &["network", "inspect", "default"])?;
    let Some(network) = parse_darwin_network(&network_output.stdout)? else {
        return Ok(false);
    };
    let routes = darwin_split_routes(&network.subnet)?;
    let ifconfig = run_required_output("ifconfig", &[])?;
    let Some(vmnet_interface) = vmnet_interface(&ifconfig.stdout, &network.gateway) else {
        return Err(LaunchError::DarwinVmnetInterfaceMissing {
            gateway: network.gateway,
        });
    };

    let mut announced = false;
    for route in routes {
        let current = run_output("route", &["-n", "get", &route.probe])?;
        if current.status.success()
            && route_interface(&current.stdout).as_deref() == Some(vmnet_interface.as_str())
        {
            continue;
        }
        if !announced {
            let mut stderr = io::stderr().lock();
            writeln!(
                stderr,
                "Adding vmnet route (VPN detected on {default_interface})"
            )?;
            announced = true;
        }
        let status = ProcessCommand::new("sudo")
            .args([
                "route",
                "add",
                "-net",
                &route.network,
                "-interface",
                &vmnet_interface,
            ])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;
        if !status.success() {
            let current = run_output("route", &["-n", "get", &route.probe])?;
            if !current.status.success()
                || route_interface(&current.stdout).as_deref() != Some(vmnet_interface.as_str())
            {
                return Err(LaunchError::DarwinVmnetRouteFailed {
                    route: route.network,
                    interface: vmnet_interface,
                });
            }
        }
    }

    Ok(true)
}

fn parse_darwin_network(stdout: &[u8]) -> Result<Option<DarwinNetwork>, LaunchError> {
    let value = serde_json::from_slice::<Value>(stdout)
        .map_err(|source| LaunchError::DarwinNetworkJson { source })?;
    let network = value
        .as_array()
        .and_then(|networks| networks.first())
        .unwrap_or(&value);
    let Some(subnet) = network
        .pointer("/status/ipv4Subnet")
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let Some(gateway) = network
        .pointer("/status/ipv4Gateway")
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    Ok(Some(DarwinNetwork {
        subnet: subnet.to_owned(),
        gateway: gateway.to_owned(),
    }))
}

fn darwin_split_routes(subnet: &str) -> Result<[DarwinSplitRoute; 2], LaunchError> {
    let Some((network, prefix)) = subnet.split_once('/') else {
        return Err(LaunchError::DarwinNetworkSubnet {
            subnet: subnet.to_owned(),
        });
    };
    let Ok(address) = network.parse::<Ipv4Addr>() else {
        return Err(LaunchError::DarwinNetworkSubnet {
            subnet: subnet.to_owned(),
        });
    };
    if prefix != "24" || address.octets()[3] != 0 {
        return Err(LaunchError::DarwinNetworkSubnet {
            subnet: subnet.to_owned(),
        });
    }
    let [first, second, third, _last] = address.octets();
    Ok([
        DarwinSplitRoute {
            network: format!("{first}.{second}.{third}.0/25"),
            probe: format!("{first}.{second}.{third}.2"),
        },
        DarwinSplitRoute {
            network: format!("{first}.{second}.{third}.128/25"),
            probe: format!("{first}.{second}.{third}.130"),
        },
    ])
}

fn route_interface(stdout: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stdout).lines().find_map(|line| {
        line.trim()
            .strip_prefix("interface:")
            .map(str::trim)
            .filter(|interface| !interface.is_empty())
            .map(str::to_owned)
    })
}

fn vmnet_interface(stdout: &[u8], gateway: &str) -> Option<String> {
    let mut interface = None;
    for line in String::from_utf8_lossy(stdout).lines() {
        if !line.starts_with(char::is_whitespace)
            && let Some((name, _rest)) = line.split_once(':')
        {
            interface = Some(name.to_owned());
            continue;
        }
        let mut fields = line.split_whitespace();
        if fields.next() == Some("inet") && fields.next() == Some(gateway) {
            return interface;
        }
    }
    None
}

fn status_to_exit(status: std::process::ExitStatus) -> Result<ExitCode, LaunchError> {
    let Some(code) = status.code() else {
        return Ok(ExitCode::FAILURE);
    };
    let code = u8::try_from(code).map_err(|_source| LaunchError::InvalidExitStatus { code })?;
    Ok(ExitCode::from(code))
}

fn current_uid() -> Result<String, LaunchError> {
    let output = run_required_output("id", &["-u"])?;
    Ok(trim_stdout(&output.stdout))
}

fn trim_stdout(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout).trim().to_owned()
}

fn mapping_env(
    mappings: &[(String, String)],
    is_read_only: impl Fn(&str) -> bool,
) -> Option<String> {
    (!mappings.is_empty()).then(|| {
        mappings
            .iter()
            .map(|(source, dest)| {
                let mode = if is_read_only(source) { "ro" } else { "rw" };
                format!("{source}:{dest}:{mode}")
            })
            .collect::<Vec<_>>()
            .join(",")
    })
}

fn mapping_uses_mount(source: &str, container: &str) -> bool {
    source == container
        || source
            .strip_prefix(container)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn first_configured_value(values: &[Option<String>]) -> Option<String> {
    values.iter().find_map(Clone::clone)
}

fn resolve_runtime_secret_env(
    security: &Security,
    spawn_env: &[(String, String)],
) -> Result<Vec<(String, String)>, LaunchError> {
    let mut resolved = Vec::new();
    for (name, policy) in &security.runtime_secrets {
        if spawn_env.iter().any(|(key, _value)| key == name.as_str()) {
            continue;
        }
        match env::var(name.as_str()) {
            Ok(value) => resolved.push((name.as_str().to_owned(), value)),
            Err(env::VarError::NotPresent) if *policy == RuntimeSecretPolicy::Optional => {}
            Err(env::VarError::NotPresent) => {
                return Err(LaunchError::RequiredRuntimeSecretMissing { name: name.clone() });
            }
            Err(env::VarError::NotUnicode(_value)) => {
                return Err(LaunchError::RuntimeSecretNotUnicode { name: name.clone() });
            }
        }
    }
    Ok(resolved)
}

fn resolve_runtime_passthrough_env() -> Result<Vec<(String, String)>, LaunchError> {
    const PASSTHROUGH: [&str; 4] = [
        "WRIX_MCP",
        "WRIX_MCP_TMUX_AUDIT",
        "WRIX_MCP_TMUX_AUDIT_FULL",
        "WRIX_VERBOSE",
    ];
    let mut resolved = Vec::new();
    for name in PASSTHROUGH {
        match env::var(name) {
            Ok(value) => resolved.push((name.to_owned(), value)),
            Err(env::VarError::NotPresent) => {}
            Err(env::VarError::NotUnicode(_value)) => {
                return Err(LaunchError::RuntimeEnvironmentNotUnicode { name });
            }
        }
    }
    Ok(resolved)
}

fn env_value(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_value) => None,
        Err(_error) => None,
    }
}

fn git_global_config(key: &str) -> Option<String> {
    let output = match ProcessCommand::new("git")
        .args(["config", "--global", "--get", key])
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) => output,
        Err(_error) => return None,
    };
    if !output.status.success() {
        return None;
    }
    let value = trim_stdout(&output.stdout);
    (!value.is_empty()).then_some(value)
}

fn env_flag(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| value != "0")
}

const fn linux_podman_network() -> &'static str {
    "pasta:--map-host-loopback,169.254.1.2,--map-guest-addr,none,-t,none,-u,none,-T,none,-U,none"
}

fn notification_env(platform: Platform) -> Option<(String, String)> {
    match platform {
        Platform::Darwin => Some((
            String::from("WRIX_NOTIFY_TCP"),
            String::from(DARWIN_NOTIFY_TCP_ENDPOINT),
        )),
        Platform::Linux => None,
    }
}

fn linux_notification_socket_mount() -> Result<Option<RenderedMount>, LaunchError> {
    linux_notification_socket_mount_from_dir(&notification_socket_dir())
}

fn notification_socket_dir() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(|| home_dir().join(".local/share"), PathBuf::from)
        .join("wrix")
}

fn linux_notification_socket_mount_from_dir(
    socket_dir: &Path,
) -> Result<Option<RenderedMount>, LaunchError> {
    let socket = socket_dir.join(NOTIFY_SOCKET_NAME);
    if !socket.try_exists()? || !file_type_is_socket(&socket)? {
        return Ok(None);
    }
    Ok(Some(RenderedMount {
        host: socket_dir.display().to_string(),
        container: String::from(LINUX_NOTIFY_CONTAINER_DIR),
        mode: MountMode::Rw,
        optional: false,
    }))
}

fn host_podman_socket_from_env() -> Result<Option<HostPodmanSocket>, LaunchError> {
    if !env_flag(UNSAFE_PODMAN_SOCKET_ENV) {
        return Ok(None);
    }
    host_podman_socket_from_flag(true, &podman_runtime_dir()?)
}

fn host_podman_socket_from_flag(
    enabled: bool,
    runtime_dir: &Path,
) -> Result<Option<HostPodmanSocket>, LaunchError> {
    if !enabled {
        return Ok(None);
    }
    let source = runtime_dir.join("podman/podman.sock");
    if !source.try_exists()? || !file_type_is_socket(&source)? {
        return Err(LaunchError::UnsafePodmanSocketMissing {
            path: source.display().to_string(),
        });
    }
    Ok(Some(HostPodmanSocket { source }))
}

fn podman_runtime_dir() -> Result<PathBuf, LaunchError> {
    if let Some(value) = env::var_os("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(value));
    }
    Ok(PathBuf::from(format!("/run/user/{}", current_uid()?)))
}

fn tmux_session_id() -> Option<SessionId> {
    env::var_os("TMUX")?;
    let output = match run_output(
        "tmux",
        &[
            "display-message",
            "-p",
            "#{session_name}:#{window_index}.#{pane_index}",
        ],
    ) {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(error = %error, "could not query tmux session");
            return None;
        }
    };
    if !output.status.success() {
        tracing::warn!(
            status = ?output.status.code(),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "tmux session query failed"
        );
        return None;
    }
    let value = trim_stdout(&output.stdout);
    match SessionId::parse(&value) {
        Ok(session_id) => Some(session_id),
        Err(error) => {
            tracing::warn!(error = %error, "tmux returned an invalid session identifier");
            None
        }
    }
}

fn session_directory(platform: Platform) -> PathBuf {
    let base = match platform {
        Platform::Linux => env::var_os("XDG_RUNTIME_DIR"),
        Platform::Darwin => env::var_os("XDG_DATA_HOME"),
    };
    base.map_or_else(|| home_dir().join(".local/share"), PathBuf::from)
        .join("wrix/sessions")
}

fn focus_target(platform: Platform) -> Option<String> {
    let (program, args): (&str, &[&str]) = match platform {
        Platform::Linux => ("niri", &["msg", "-j", "focused-window"]),
        Platform::Darwin => (
            "osascript",
            &[
                "-e",
                "tell application \"System Events\" to name of first process whose frontmost is true",
            ],
        ),
    };
    let output = match run_output(program, args) {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(program = %program, error = %error, "could not query focus target");
            return None;
        }
    };
    if !output.status.success() {
        tracing::warn!(
            program = %program,
            status = ?output.status.code(),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "focus target query failed"
        );
        return None;
    }
    match platform {
        Platform::Linux => {
            let value = match serde_json::from_slice::<Value>(&output.stdout) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(program = %program, error = %error, "focus target returned invalid JSON");
                    return None;
                }
            };
            match value.get("id") {
                Some(Value::String(text)) => Some(text.clone()),
                Some(Value::Number(number)) => Some(number.to_string()),
                Some(value) => {
                    tracing::warn!(program = %program, ?value, "focus target returned an invalid window identifier");
                    None
                }
                None => {
                    tracing::warn!(program = %program, "focus target omitted the window identifier");
                    None
                }
            }
        }
        Platform::Darwin => {
            let target = trim_stdout(&output.stdout);
            if target.is_empty() {
                tracing::warn!(program = %program, "focus target returned an empty application name");
                None
            } else {
                Some(target)
            }
        }
    }
}

fn terminal_size() -> Option<(u32, u32)> {
    let terminal = match fs::File::open("/dev/tty") {
        Ok(terminal) => terminal,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(error = %error, "could not open terminal for size query");
            return None;
        }
    };
    let output = match ProcessCommand::new("stty")
        .arg("size")
        .stdin(terminal)
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(error = %error, "could not run terminal size query");
            return None;
        }
    };
    if !output.status.success() {
        tracing::warn!(
            status = ?output.status.code(),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "terminal size query failed"
        );
        return None;
    }
    let text = trim_stdout(&output.stdout);
    let mut dimensions = text.split_whitespace();
    let rows = parse_terminal_dimension(dimensions.next(), "rows")?;
    let columns = parse_terminal_dimension(dimensions.next(), "columns")?;
    Some((rows, columns))
}

fn parse_terminal_dimension(value: Option<&str>, dimension: &'static str) -> Option<u32> {
    let Some(value) = value else {
        tracing::warn!(dimension = %dimension, "terminal size query omitted a dimension");
        return None;
    };
    match value.parse::<u32>() {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(
                dimension = %dimension,
                value = %value,
                error = %error,
                "terminal size query returned an invalid dimension"
            );
            None
        }
    }
}

fn serialize_shell_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| format!("'{}'", arg.replace('\'', "'\"'\"'")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn ensure_krun() -> Result<(), LaunchError> {
    require_kvm(Path::new("/dev/kvm"))?;
    if matches!(run_output("krun", &["--version"]), Ok(output) if output.status.success()) {
        return Ok(());
    }
    let podman_has_krun = match run_output(
        "podman",
        &[
            "info",
            "--format",
            "{{range .Host.OCIRuntime.Alternatives}}{{.}}{{end}}",
        ],
    ) {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).contains("krun")
        }
        Ok(_output) => false,
        Err(_error) => false,
    };
    if podman_has_krun {
        Ok(())
    } else {
        Err(LaunchError::KrunMissing)
    }
}

fn require_kvm(path: &Path) -> Result<(), LaunchError> {
    if path.exists() {
        Ok(())
    } else {
        Err(LaunchError::KvmMissing)
    }
}

fn deploy_key_name(workspace: &Path, configured: Option<&KeyName>) -> Result<KeyName, LaunchError> {
    if let Some(name) = configured {
        return Ok(name.clone());
    }
    let workspace_name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace");
    KeyName::parse(&format!("{}-{}", workspace_name, host_label()))
        .map_err(|source| LaunchError::InvalidDerivedDeployKeyName { source })
}

fn host_label() -> String {
    if let Ok(value) = env::var("HOSTNAME")
        && let Some(label) = first_host_label(&value)
    {
        return label;
    }
    for (program, args) in [
        ("hostname", ["-s"].as_slice()),
        ("uname", ["-n"].as_slice()),
    ] {
        if let Ok(output) = ProcessCommand::new(program).args(args).output()
            && output.status.success()
        {
            let value = trim_stdout(&output.stdout);
            if let Some(label) = first_host_label(&value) {
                return label;
            }
        }
    }
    String::from("localhost")
}

fn first_host_label(value: &str) -> Option<String> {
    value
        .split('.')
        .next()
        .filter(|label| !label.is_empty())
        .map(ToOwned::to_owned)
}

fn resolve_key(env_name: &str, name: &str, signing: bool) -> Result<Option<PathBuf>, LaunchError> {
    if let Some(value) = env::var_os(env_name) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(Some(path));
        }
        let path_text = path.display().to_string();
        return if signing {
            Err(LaunchError::SigningKeyMissing { path: path_text })
        } else {
            Err(LaunchError::DeployKeyMissing { path: path_text })
        };
    }
    let path = default_key_path(name);
    Ok(path.is_file().then_some(path))
}

fn default_key_path(name: &str) -> PathBuf {
    home_dir().join(".ssh/deploy_keys").join(name)
}

fn home_dir() -> PathBuf {
    env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn expand_path(input: &str) -> Result<PathBuf, LaunchError> {
    let mut expanded = input.to_owned();
    if input.starts_with('~') || input.contains("$HOME") {
        let home = expansion_env("HOME")?;
        if let Some(suffix) = expanded.strip_prefix('~') {
            expanded = format!("{home}{suffix}");
        }
        expanded = expanded.replace("$HOME", &home);
    }
    if input.contains("$USER") {
        expanded = expanded.replace("$USER", &expansion_env("USER")?);
    }
    Ok(PathBuf::from(expanded))
}

fn expansion_env(name: &'static str) -> Result<String, LaunchError> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        Ok(_) | Err(env::VarError::NotPresent) => {
            Err(LaunchError::PathExpansionEnvMissing { name })
        }
        Err(env::VarError::NotUnicode(_value)) => {
            Err(LaunchError::PathExpansionEnvNotUnicode { name })
        }
    }
}

fn stable_mount_index(path: &Path) -> u64 {
    path.as_os_str()
        .to_string_lossy()
        .bytes()
        .fold(0_u64, |state, byte| {
            state.wrapping_mul(33).wrapping_add(u64::from(byte))
        })
}

#[cfg(test)]
mod test {
    use std::path::{Path, PathBuf};

    use wrix_core::deploy_key::Name as KeyName;

    use super::{
        DarwinMounts, DarwinNetwork, DarwinSplitRoute, HostPodmanSocket, LaunchError, NetworkMode,
        RenderedMount, SessionId, SessionRegistration, Staging, darwin_split_routes,
        deploy_key_name, linux_podman_network, parse_darwin_network, route_interface,
        vmnet_interface,
    };
    use crate::command::config::{MountMode, Platform, ProfileMount, SpawnMount};

    #[test]
    fn launcher_cache_key_reader_rejects_malformed_encoding_and_config_injection() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cache.pub");
        assert!(matches!(
            super::read_cache_public_key(&path),
            Err(LaunchError::MissingCachePublicKey { .. })
        ));
        for key in [
            "cache:!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!=",
            "cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=",
            "cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\ntrusted-users = root",
            "cache\ntrusted-users=root:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        ] {
            std::fs::write(&path, key).unwrap();
            assert!(
                matches!(
                    super::read_cache_public_key(&path),
                    Err(LaunchError::InvalidCachePublicKey { .. })
                ),
                "accepted {key:?}"
            );
        }
        for key in [
            "cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            std::fs::write(&path, format!("{key}\n")).unwrap();
            assert_eq!(super::read_cache_public_key(&path).unwrap().as_str(), key);
        }
    }

    #[test]
    fn path_expansion_keeps_shell_syntax_literal() {
        for input in [
            "",
            "/foo/~/bar",
            "/absolute/path",
            "$PATH/bin",
            "$MALICIOUS",
            "$(whoami)",
            "`id`",
            "/tmp/$(rm -rf /)/file",
            "$((1+1))",
            "{a,b,c}",
        ] {
            assert_eq!(super::expand_path(input).unwrap(), PathBuf::from(input));
        }
    }

    #[test]
    fn spawn_mount_renders_ro_only_when_requested() {
        let rw = RenderedMount::from_spawn(&SpawnMount {
            host_path: String::from("/host/rw"),
            container_path: String::from("/mnt/rw"),
            read_only: false,
        });
        let ro = RenderedMount::from_spawn(&SpawnMount {
            host_path: String::from("/host/ro"),
            container_path: String::from("/mnt/ro"),
            read_only: true,
        });
        assert_eq!(rw.mode, MountMode::Rw);
        assert_eq!(rw.podman_arg(), "/host/rw:/mnt/rw");
        assert_eq!(ro.podman_arg(), "/host/ro:/mnt/ro:ro");
    }

    #[test]
    fn linux_network_disables_pasta_auto_forwarding() {
        let network = linux_podman_network();
        assert!(network.contains("--map-host-loopback,169.254.1.2"));
        assert!(network.contains("-t,none"));
        assert!(!network.contains("-t,auto"));
    }

    #[test]
    fn darwin_notification_env_uses_vmnet_gateway() {
        assert_eq!(
            super::notification_env(Platform::Darwin),
            Some((
                String::from("WRIX_NOTIFY_TCP"),
                String::from("192.168.64.1:5959")
            ))
        );
        assert_eq!(super::notification_env(Platform::Linux), None);
    }

    #[test]
    fn darwin_network_parses_apple_inspect_array() {
        let inspect = br#"[
          {
            "status": {
              "ipv4Gateway": "192.168.64.1",
              "ipv4Subnet": "192.168.64.0/24"
            }
          }
        ]"#;
        assert_eq!(
            parse_darwin_network(inspect).unwrap(),
            Some(DarwinNetwork {
                subnet: String::from("192.168.64.0/24"),
                gateway: String::from("192.168.64.1"),
            })
        );
    }

    #[test]
    fn darwin_vmnet_route_parsers_find_interfaces_and_split_subnet() {
        let default_route = b"   route to: default\n  interface: utun11\n";
        let ifconfig = b"en0: flags=8863<UP>\n\tinet 192.168.1.2 netmask 0xffffff00\nbridge100: flags=8a63<UP>\n\tinet 192.168.64.1 netmask 0xffffff00\n";
        assert_eq!(route_interface(default_route).as_deref(), Some("utun11"));
        assert_eq!(
            vmnet_interface(ifconfig, "192.168.64.1").as_deref(),
            Some("bridge100")
        );
        assert_eq!(
            darwin_split_routes("192.168.64.0/24").unwrap(),
            [
                DarwinSplitRoute {
                    network: String::from("192.168.64.0/25"),
                    probe: String::from("192.168.64.2"),
                },
                DarwinSplitRoute {
                    network: String::from("192.168.64.128/25"),
                    probe: String::from("192.168.64.130"),
                },
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn linux_notification_mount_uses_socket_directory() {
        use std::os::unix::net::UnixListener;

        let root = scratch_dir("notify-mount");
        let socket_dir = root.join("wrix");
        std::fs::create_dir_all(&socket_dir).unwrap();
        let _listener = UnixListener::bind(socket_dir.join("notify.sock")).unwrap();

        let mount = super::linux_notification_socket_mount_from_dir(&socket_dir)
            .unwrap()
            .unwrap();
        assert_eq!(mount.host, socket_dir.display().to_string());
        assert_eq!(mount.container, "/run/wrix");
        assert_eq!(mount.mode, MountMode::Rw);
    }

    #[test]
    fn network_mode_parse_accepts_only_open_and_limit() {
        assert_eq!(NetworkMode::parse("open"), Some(NetworkMode::Open));
        assert_eq!(NetworkMode::parse("limit"), Some(NetworkMode::Limit));
        assert_eq!(NetworkMode::parse("lan"), None);
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_podman_socket_mounts_only_on_explicit_opt_in() {
        use std::os::unix::net::UnixListener;

        let runtime_dir = scratch_dir("unsafe-podman");
        let socket_dir = runtime_dir.join("podman");
        std::fs::create_dir_all(&socket_dir).unwrap();
        let socket_path = socket_dir.join("podman.sock");
        let _listener = UnixListener::bind(&socket_path).unwrap();

        assert!(
            super::host_podman_socket_from_flag(false, &runtime_dir)
                .unwrap()
                .is_none()
        );

        let socket = super::host_podman_socket_from_flag(true, &runtime_dir)
            .unwrap()
            .unwrap();
        assert_eq!(socket.source, socket_path);
        assert_eq!(
            socket.mount().podman_arg(),
            format!("{}:/run/podman/podman.sock", socket_path.display())
        );
        let pairs = HostPodmanSocket::env_pairs(
            Path::new("/host/workspace"),
            Some(Path::new("/host/beads")),
        );
        assert!(pairs.contains(&(
            String::from("CONTAINER_HOST"),
            String::from("unix:///run/podman/podman.sock")
        )));
        assert!(pairs.contains(&(
            String::from("GC_HOST_WORKSPACE"),
            String::from("/host/workspace")
        )));
        assert!(pairs.contains(&(String::from("GC_HOST_BEADS"), String::from("/host/beads"))));
    }

    #[test]
    fn unsafe_podman_socket_missing_fails_loud() {
        let runtime_dir = scratch_dir("unsafe-podman-missing");
        let error = super::host_podman_socket_from_flag(true, &runtime_dir).unwrap_err();
        assert!(matches!(
            error,
            LaunchError::UnsafePodmanSocketMissing { path }
                if path.ends_with("podman/podman.sock")
        ));
    }

    #[test]
    fn darwin_mount_classifier_handles_profile_and_spawn_mounts() {
        let root = scratch_dir("darwin-mounts");
        let host_dir = root.join("host-dir");
        let host_file = root.join("host-file");
        let staging_root = root.join("stage");
        std::fs::create_dir_all(&host_dir).unwrap();
        std::fs::write(host_dir.join("payload"), b"dir").unwrap();
        std::fs::write(&host_file, b"file").unwrap();
        std::fs::create_dir_all(&staging_root).unwrap();

        let staging = Staging { root: staging_root };
        let profile_mount = ProfileMount {
            source: host_dir.display().to_string(),
            dest: String::from("/mnt/profile-dir"),
            mode: MountMode::Ro,
            optional: false,
        };
        let spawn_mount = SpawnMount {
            host_path: host_file.display().to_string(),
            container_path: String::from("/etc/spawn-file"),
            read_only: true,
        };
        let mut mounts = DarwinMounts::default();
        mounts
            .push(&RenderedMount::from_profile(&profile_mount), &staging.root)
            .unwrap();
        mounts
            .push(&RenderedMount::from_spawn(&spawn_mount), &staging.root)
            .unwrap();

        assert_eq!(
            mounts.dir_env().as_deref(),
            Some("/mnt/wrix/dir0:/mnt/profile-dir:ro")
        );
        assert_eq!(
            mounts.file_env().as_deref(),
            Some("/mnt/wrix/file0/host-file:/etc/spawn-file:ro")
        );
        assert_eq!(mounts.mounts.len(), 2);
        assert!(
            mounts
                .mounts
                .iter()
                .all(|mount| mount.mode == MountMode::Ro)
        );
        assert_eq!(
            mounts.mounts[1].host,
            staging.root.join("file0").display().to_string()
        );
    }

    #[test]
    fn darwin_file_sync_updates_only_requested_rw_sources() {
        let root = scratch_dir("darwin-file-sync");
        let host_dir = root.join("host");
        let staging_root = root.join("stage");
        std::fs::create_dir_all(&host_dir).unwrap();
        std::fs::create_dir_all(&staging_root).unwrap();
        let read_only = host_dir.join("read-only");
        let writable = host_dir.join("writable");
        let sibling = host_dir.join("sibling-secret");
        std::fs::write(&read_only, b"read-only original\n").unwrap();
        std::fs::write(&writable, b"writable original\n").unwrap();
        std::fs::write(&sibling, b"private\n").unwrap();

        let mut mounts = DarwinMounts::default();
        mounts
            .push(
                &RenderedMount {
                    host: read_only.display().to_string(),
                    container: String::from("/etc/read-only"),
                    mode: MountMode::Ro,
                    optional: false,
                },
                &staging_root,
            )
            .unwrap();
        mounts
            .push(
                &RenderedMount {
                    host: writable.display().to_string(),
                    container: String::from("/etc/writable"),
                    mode: MountMode::Rw,
                    optional: false,
                },
                &staging_root,
            )
            .unwrap();

        std::fs::write(staging_root.join("file0/read-only"), b"ignored\n").unwrap();
        std::fs::write(staging_root.join("file1/writable"), b"updated\n").unwrap();
        mounts.sync_files().unwrap();

        assert_eq!(std::fs::read(&read_only).unwrap(), b"read-only original\n");
        assert_eq!(std::fs::read(&writable).unwrap(), b"updated\n");
        assert_eq!(std::fs::read(&sibling).unwrap(), b"private\n");
    }

    #[test]
    fn missing_optional_profile_mount_is_skipped_by_platform_planners() {
        let root = scratch_dir("optional-profile-mount");
        let staging_root = root.join("stage");
        std::fs::create_dir_all(&staging_root).unwrap();
        let staging = Staging { root: staging_root };
        let profile_mounts = [ProfileMount {
            source: root.join("missing").display().to_string(),
            dest: String::from("/mnt/optional"),
            mode: MountMode::Rw,
            optional: true,
        }];

        assert!(
            super::render_profile_mount(&profile_mounts[0], &staging)
                .unwrap()
                .is_none()
        );
        let darwin =
            super::darwin_mounts_from_rendered(&profile_mounts, &[], &staging.root).unwrap();
        assert!(darwin.mounts.is_empty());
        assert!(darwin.dir_mappings.is_empty());
        assert!(darwin.file_mappings.is_empty());
    }

    #[test]
    fn missing_required_profile_mount_is_rejected_by_platform_planners() {
        let root = scratch_dir("required-profile-mount");
        let staging_root = root.join("stage");
        std::fs::create_dir_all(&staging_root).unwrap();
        let staging = Staging { root: staging_root };
        let profile_mounts = [ProfileMount {
            source: root.join("missing").display().to_string(),
            dest: String::from("/mnt/required"),
            mode: MountMode::Rw,
            optional: false,
        }];

        let linux_error = super::render_profile_mount(&profile_mounts[0], &staging).unwrap_err();
        assert!(matches!(
            linux_error,
            LaunchError::MountSourceMissing { .. }
        ));
        assert!(matches!(
            super::darwin_mounts_from_rendered(&profile_mounts, &[], &staging.root),
            Err(LaunchError::MountSourceMissing { .. })
        ));
    }

    #[test]
    fn session_id_accepts_only_tmux_target_shape() {
        assert!(SessionId::parse("main:2.1").is_ok());
        assert!(SessionId::parse("space.name:with:colons:20.11").is_ok());
        for invalid in ["", "main", ":2.1", "main:x.1", "main:2.x", "main:2.1.0"] {
            assert!(SessionId::parse(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn session_registration_uses_daemon_filename_and_removes_current_file() {
        let root = scratch_dir("session-registration");
        let session_id = SessionId::parse("main:2.1").unwrap();
        let registration =
            SessionRegistration::create_in(Some(&session_id), Platform::Linux, &root).unwrap();
        let path = root.join("main-2-1.json");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

        assert_eq!(
            value.get("session_id").and_then(serde_json::Value::as_str),
            Some("main:2.1")
        );
        assert!(value.get("window_id").is_some());
        registration.remove().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn overlapping_session_registrations_remove_file_after_last_cleanup() {
        let root = scratch_dir("overlapping-session-registration");
        let session_id = SessionId::parse("main:2.1").unwrap();
        let first =
            SessionRegistration::create_in(Some(&session_id), Platform::Linux, &root).unwrap();
        let second =
            SessionRegistration::create_in(Some(&session_id), Platform::Linux, &root).unwrap();
        let path = root.join("main-2-1.json");

        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            value
                .get("registration_count")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );

        first.remove().unwrap();
        assert!(path.exists());
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            value
                .get("registration_count")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );

        second.remove().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn stable_mount_index_is_deterministic() {
        assert_eq!(
            super::stable_mount_index(&PathBuf::from("/tmp/a")),
            super::stable_mount_index(&PathBuf::from("/tmp/a"))
        );
    }

    #[test]
    fn microvm_opt_in_without_kvm_fails_loudly() {
        let missing = scratch_dir("missing-kvm").join("dev/kvm");
        assert!(matches!(
            super::require_kvm(&missing),
            Err(LaunchError::KvmMissing)
        ));
    }

    #[test]
    fn deploy_key_name_prefers_profile_config_value() {
        let configured = KeyName::parse("custom-key").unwrap();
        let name = deploy_key_name(Path::new("/workspace/repo"), Some(&configured)).unwrap();
        assert_eq!(name.as_str(), "custom-key");
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("wrix-sandbox-{name}-{}", std::process::id()));
        if root.exists() {
            std::fs::remove_dir_all(&root).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}
