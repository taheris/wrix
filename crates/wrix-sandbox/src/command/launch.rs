use std::{
    collections::BTreeSet,
    env, fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode, Output, Stdio},
};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use wrix_core::path::Workspace;

use super::config::{
    AgentKind, MountMode, Platform, ProfileConfig, ProfileMount, SourceKind, SpawnConfig,
    SpawnMount,
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
    /// project cache public key is invalid: {path}
    InvalidCachePublicKey { path: String },
    /// service command failed: {stderr}
    ServiceFailed { stderr: String },
    /// command failed: {program}: {stderr}
    ProcessFailed { program: String, stderr: String },
    /// nix-descriptor image source is missing a sha256 digest: {path}
    MissingDescriptorDigest { path: String },
    /// nix-descriptor image source is missing oci_layout: {path}
    MissingDescriptorLayout { path: String },
    /// docker-archive image source is missing a sha256 digest: {path}
    MissingDockerArchiveDigest { path: String },
    /// unsupported image source_kind: {kind}
    UnsupportedImageSourceKind { kind: &'static str },
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
    /// WRIX_PI_AUTH_FILE={path}: file does not exist
    PiAuthMissing { path: String },
    /// wrix spawn: Pi auth file not found at {path} — run 'pi' and /login on the host, or set WRIX_PI_AUTH_FILE to an existing auth.json
    SpawnPiAuthMissing { path: String },
    /// WRIX_UNSAFE_PODMAN_SOCKET set but socket not found at {path}
    UnsafePodmanSocketMissing { path: String },
    /// /dev/kvm not found. A microVM boundary requires KVM support.
    KvmMissing,
    /// krun runtime not found. A microVM boundary requires crun with libkrun.
    KrunMissing,
    /// {source}
    Io { source: io::Error },
    /// invalid service endpoint JSON: {source}
    ServiceJson { source: serde_json::Error },
    /// invalid image MRU JSON: {source}
    ImageMruJson { source: serde_json::Error },
    /// invalid image descriptor JSON: {source}
    DescriptorJson { source: serde_json::Error },
}

impl From<io::Error> for LaunchError {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

pub fn execute(request: &Request, stdout: &mut impl Write) -> Result<ExitCode, LaunchError> {
    let dry_run = env_flag("WRIX_DRY_RUN");
    let services = if !dry_run || env_flag("WRIX_DRY_RUN_SERVICES") {
        ServicesState::load(request)?
    } else {
        ServicesState::default()
    };
    let plan = Plan::new(request, services)?;
    if dry_run {
        plan.write_dry_run(stdout)?;
        return Ok(ExitCode::SUCCESS);
    }
    plan.launch()
}

struct Plan<'a> {
    request: &'a Request,
    workspace: PathBuf,
    image_ref: String,
    image_source: String,
    image_source_kind: SourceKind,
    image_digest: String,
    stdio: bool,
    agent_args: Vec<String>,
    spawn_env: Vec<(String, String)>,
    spawn_mounts: Vec<RenderedMount>,
    services: ServicesState,
    host_podman_socket: Option<HostPodmanSocket>,
    network_mode: NetworkMode,
    git_identity: GitIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NetworkMode {
    Open,
    Limit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageRuntime {
    Podman,
    Container,
}

const DARWIN_NOTIFY_TCP_ENDPOINT: &str = "192.168.64.1:5959";
const LINUX_NOTIFY_CONTAINER_DIR: &str = "/run/wrix";
const NOTIFY_SOCKET_NAME: &str = "notify.sock";
const PODMAN_SOCKET_CONTAINER_PATH: &str = "/run/podman/podman.sock";
const UNSAFE_PODMAN_SOCKET_ENV: &str = "WRIX_UNSAFE_PODMAN_SOCKET";

#[derive(Clone, Debug)]
struct RenderedMount {
    host: String,
    container: String,
    mode: MountMode,
}

#[derive(Default)]
struct DarwinMounts {
    mounts: Vec<RenderedMount>,
    dir_mappings: Vec<(String, String)>,
    file_mappings: Vec<(String, String)>,
}

#[derive(Default)]
struct ServicesState {
    project_cache_url: Option<String>,
    project_cache_host: Option<String>,
    project_cache_port: Option<u16>,
    project_cache_nix_config: Option<String>,
    beads_socket: Option<BeadsSocket>,
    beads_tcp: Option<BeadsTcp>,
}

struct BeadsSocket {
    container_socket: String,
    mount_source: Option<PathBuf>,
}

struct BeadsTcp {
    host: String,
    port: u16,
}

#[derive(Clone, Debug)]
struct HostPodmanSocket {
    source: PathBuf,
}

struct ImageSource {
    ref_name: String,
    source: String,
    kind: SourceKind,
    digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ImageRecord {
    #[serde(default, rename = "ref", skip_serializing_if = "String::is_empty")]
    ref_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    digest: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
}

impl NetworkMode {
    fn from_env() -> Result<Self, LaunchError> {
        match env::var("WRIX_NETWORK") {
            Ok(value) => Self::parse(&value).ok_or(LaunchError::InvalidNetworkMode { value }),
            Err(env::VarError::NotPresent) => Ok(Self::Open),
            Err(env::VarError::NotUnicode(value)) => Err(LaunchError::InvalidNetworkMode {
                value: value.to_string_lossy().into_owned(),
            }),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Limit => "limit",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"open" => Some(Self::Open),
            b"limit" => Some(Self::Limit),
            _ => None,
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
    fn new(request: &'a Request, services: ServicesState) -> Result<Self, LaunchError> {
        let network_mode = NetworkMode::from_env()?;

        let profile = &request.profile_config;
        let source = match &request.kind {
            Kind::Run(_run) => ImageSource {
                ref_name: profile.image.reference.clone(),
                source: profile.image.source.clone(),
                kind: profile.image.source_kind,
                digest: profile.image.digest.clone(),
            },
            Kind::Spawn(spawn) => {
                let config = &spawn.config;
                let ref_name = non_empty_override(config.image_ref.as_deref())
                    .unwrap_or(profile.image.reference.as_str())
                    .to_owned();
                let source = non_empty_override(config.image_source.as_deref())
                    .unwrap_or(profile.image.source.as_str())
                    .to_owned();
                let kind = config
                    .image_source_kind
                    .unwrap_or(profile.image.source_kind);
                let digest = if ref_name == profile.image.reference
                    && source == profile.image.source
                    && kind == profile.image.source_kind
                {
                    profile.image.digest.clone()
                } else {
                    String::new()
                };
                ImageSource {
                    ref_name,
                    source,
                    kind,
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
                    .map(|pair| (pair[0].clone(), pair[1].clone()))
                    .collect(),
                spawn
                    .config
                    .mounts
                    .iter()
                    .map(RenderedMount::from_spawn)
                    .collect(),
            ),
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
            image_source_kind: source.kind,
            image_digest: source.digest,
            stdio,
            agent_args,
            spawn_env,
            spawn_mounts,
            services,
            host_podman_socket,
            network_mode,
            git_identity: GitIdentity::load(),
        })
    }

    fn write_dry_run(&self, stdout: &mut impl Write) -> Result<(), LaunchError> {
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
        if let Some(socket) = &self.host_podman_socket {
            socket.write_dry_run(stdout, &self.workspace, None)?;
        }
        if let Kind::Spawn(spawn) = &self.request.kind {
            writeln!(stdout, "SPAWN_CONFIG={}", spawn.config_path.display())?;
        }
        for (key, value) in &self.spawn_env {
            writeln!(stdout, "ENV={key}={value}")?;
        }
        for arg in &self.agent_args {
            writeln!(stdout, "CMD={arg}")?;
        }
        for mount in &self.spawn_mounts {
            writeln!(stdout, "MOUNT=-v {}", mount.podman_arg())?;
        }
        if Platform::CURRENT == Platform::Darwin {
            let staging = Staging::create()?;
            let mounts = self.darwin_mounts(&staging)?;
            mounts.write_dry_run(stdout)?;
        }
        Ok(())
    }

    fn launch(&self) -> Result<ExitCode, LaunchError> {
        match Platform::CURRENT {
            Platform::Linux => self.launch_linux(),
            Platform::Darwin => self.launch_darwin(),
        }
    }

    fn launch_linux(&self) -> Result<ExitCode, LaunchError> {
        install_linux_image(
            &self.image_ref,
            &self.image_source,
            self.image_source_kind,
            &self.image_digest,
        )?;
        self.remember_and_prune_images(ImageRuntime::Podman)?;
        let staging = Staging::create()?;
        let mut volumes = self.linux_volumes(&staging)?;
        let credentials = self.credentials(&staging)?;
        if let Some(credentials) = &credentials {
            volumes.push(credentials.mount());
        }
        let pi_auth = self.pi_auth()?;
        if let Some(pi_auth) = &pi_auth {
            volumes.push(pi_auth.mount.clone());
        }
        let staged_beads = stage_beads(&self.workspace, &staging)?;
        if let Some(beads) = &staged_beads {
            volumes.push(RenderedMount {
                host: beads.display().to_string(),
                container: String::from("/workspace/.beads"),
                mode: MountMode::Rw,
            });
        }
        if let Some(socket) = &self.host_podman_socket {
            volumes.push(socket.mount());
        }

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
        if env_flag("WRIX_MICROVM") {
            ensure_krun()?;
            command.arg("--runtime").arg("krun").arg("--userns=keep-id");
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
            pi_auth.as_ref().map(|auth| auth.container_path.as_str()),
        ) {
            command.arg("-e").arg(format!("{key}={value}"));
        }
        for (key, value) in self.host_podman_socket_env_pairs(staged_beads.as_deref()) {
            command.arg("-e").arg(format!("{key}={value}"));
        }
        if !env_flag("WRIX_MICROVM") {
            command.arg("-e").arg("IS_SANDBOX=1");
        }
        command
            .arg("-w")
            .arg("/workspace")
            .arg(&self.image_ref)
            .args(&self.agent_args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        Ok(status_to_exit(command.status()?))
    }

    fn launch_darwin(&self) -> Result<ExitCode, LaunchError> {
        install_darwin_image(
            &self.image_ref,
            &self.image_source,
            self.image_source_kind,
            &self.image_digest,
        )?;
        self.remember_and_prune_images(ImageRuntime::Container)?;
        let staging = Staging::create()?;
        let darwin_mounts = self.darwin_mounts(&staging)?;
        let credentials = self.credentials(&staging)?;
        let pi_auth = self.pi_auth()?;
        let mut command = ProcessCommand::new("container");
        command.arg("run").arg("--rm");
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
            command
                .arg("-v")
                .arg(format!("{}:{}", mount.host, mount.container));
        }
        if let Some(credentials) = &credentials {
            let key_mount = credentials.mount();
            command
                .arg("-v")
                .arg(format!("{}:{}", key_mount.host, key_mount.container));
        }
        if let Some(pi_auth) = &pi_auth {
            command.arg("-v").arg(pi_auth.mount.podman_arg());
        }
        for (key, value) in self.env_pairs(
            credentials.as_ref(),
            pi_auth.as_ref().map(|auth| auth.container_path.as_str()),
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
        command
            .arg("--")
            .arg(&self.image_ref)
            .args(&self.agent_args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        Ok(status_to_exit(command.status()?))
    }

    fn linux_volumes(&self, staging: &Staging) -> Result<Vec<RenderedMount>, LaunchError> {
        let mut volumes = vec![RenderedMount {
            host: self.workspace.display().to_string(),
            container: String::from("/workspace"),
            mode: MountMode::Rw,
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
            });
        }
        for mount in &self.request.profile_config.profile.mounts {
            volumes.push(render_profile_mount(mount, staging)?);
        }
        volumes.extend(self.spawn_mounts.clone());
        Ok(volumes)
    }

    fn darwin_mounts(&self, staging: &Staging) -> Result<DarwinMounts, LaunchError> {
        let mut mounts = DarwinMounts::default();
        for mount in &self.request.profile_config.profile.mounts {
            mounts.push(&RenderedMount::from_profile(mount), staging)?;
        }
        for mount in &self.spawn_mounts {
            mounts.push(mount, staging)?;
        }
        Ok(mounts)
    }

    fn remember_and_prune_images(&self, runtime: ImageRuntime) -> Result<(), LaunchError> {
        let digest = if self.image_source.is_empty() {
            String::new()
        } else {
            desired_digest(
                &self.image_source,
                self.image_source_kind,
                &self.image_digest,
            )?
        };
        remember_image_ref(runtime, &self.image_ref, &digest)?;
        prune_stale_images(runtime, &self.image_ref, &digest)
    }

    fn credentials(&self, staging: &Staging) -> Result<Option<Credentials>, LaunchError> {
        let name = deploy_key_name(
            &self.workspace,
            self.request.profile_config.security.deploy_key.as_deref(),
        );
        let deploy = resolve_key("WRIX_DEPLOY_KEY", &name, false)?;
        let signing_name = format!("{name}-signing");
        let signing = resolve_key("WRIX_SIGNING_KEY", &signing_name, true)?;
        if self.spawn() {
            if deploy.is_none() {
                return Err(LaunchError::SpawnDeployKeyMissing {
                    path: default_key_path(&name).display().to_string(),
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
        let Some(deploy_path) = deploy else {
            return Ok(None);
        };
        let key_root = staging.root.join("deploy_keys");
        fs::create_dir_all(&key_root)?;
        let deploy_target = key_root.join(&name);
        fs::copy(&deploy_path, &deploy_target)?;
        let signing_target = if let Some(path) = signing {
            let target = key_root.join(&signing_name);
            fs::copy(path, &target)?;
            Some(target)
        } else {
            None
        };
        Ok(Some(Credentials {
            deploy: deploy_target,
            signing: signing_target,
            name,
        }))
    }

    fn pi_auth(&self) -> Result<Option<PiAuth>, LaunchError> {
        if self.request.profile_config.agent.kind != AgentKind::Pi {
            return Ok(None);
        }
        let path = env::var_os("WRIX_PI_AUTH_FILE")
            .map_or_else(|| home_dir().join(".pi/agent/auth.json"), PathBuf::from);
        if env::var_os("WRIX_PI_AUTH_FILE").is_some() && !path.is_file() {
            return Err(LaunchError::PiAuthMissing {
                path: path.display().to_string(),
            });
        }
        if self.spawn() && !path.is_file() {
            return Err(LaunchError::SpawnPiAuthMissing {
                path: path.display().to_string(),
            });
        }
        if !self.spawn() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            if !path.exists() {
                fs::write(&path, b"{}\n")?;
            }
        }
        let container_path = String::from("/mnt/wrix/file/pi-auth.json");
        Ok(Some(PiAuth {
            mount: RenderedMount {
                host: path.display().to_string(),
                container: container_path.clone(),
                mode: MountMode::Rw,
            },
            container_path,
        }))
    }

    fn host_podman_socket_env_pairs(&self, staged_beads: Option<&Path>) -> Vec<(String, String)> {
        self.host_podman_socket.as_ref().map_or_else(Vec::new, |_| {
            HostPodmanSocket::env_pairs(&self.workspace, staged_beads)
        })
    }

    fn env_pairs(
        &self,
        credentials: Option<&Credentials>,
        pi_auth: Option<&str>,
    ) -> Vec<(String, String)> {
        let mut pairs = self
            .request
            .profile_config
            .profile
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        if self.spawn() {
            pairs.extend(self.spawn_env.iter().cloned());
            if self.stdio {
                pairs.push((String::from("WRIX_STDIO"), String::from("1")));
            }
        } else {
            pairs.push((
                String::from("WRIX_VERBOSE"),
                env::var("WRIX_VERBOSE").unwrap_or_default(),
            ));
            pairs.push((
                String::from("CLAUDE_CODE_OAUTH_TOKEN"),
                env::var("CLAUDE_CODE_OAUTH_TOKEN").unwrap_or_default(),
            ));
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
        ]);
        if let Some(pair) = notification_env(Platform::CURRENT) {
            pairs.push(pair);
        }
        if let Ok(value) = env::var("WRIX_GIT_SIGN") {
            pairs.push((String::from("WRIX_GIT_SIGN"), value));
        }
        if let Some(cache_host) = &self.services.project_cache_host {
            pairs.push((String::from("WRIX_PROJECT_CACHE_HOST"), cache_host.clone()));
        }
        if let Some(cache_port) = self.services.project_cache_port {
            pairs.push((
                String::from("WRIX_PROJECT_CACHE_PORT"),
                cache_port.to_string(),
            ));
        }
        if let Some(nix_config) = &self.services.project_cache_nix_config {
            pairs.push((String::from("NIX_CONFIG"), nix_config.clone()));
        }
        if let Some(beads) = &self.services.beads_socket {
            pairs.push((
                String::from("BEADS_DOLT_SERVER_SOCKET"),
                beads.container_socket.clone(),
            ));
        }
        if let Some(beads) = &self.services.beads_tcp {
            pairs.push((String::from("BEADS_DOLT_SERVER_HOST"), beads.host.clone()));
            pairs.push((
                String::from("BEADS_DOLT_SERVER_PORT"),
                beads.port.to_string(),
            ));
        }
        if let Some(auth) = pi_auth {
            pairs.push((String::from("WRIX_PI_AUTH_JSON"), auth.to_owned()));
        }
        if let Some(credentials) = credentials {
            pairs.push((
                String::from("WRIX_DEPLOY_KEY"),
                format!("/etc/wrix/keys/{}", credentials.name),
            ));
            if credentials.signing.is_some() {
                pairs.push((
                    String::from("WRIX_SIGNING_KEY"),
                    format!("/etc/wrix/keys/{}-signing", credentials.name),
                ));
            }
        }
        pairs
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
            Kind::Spawn(spawn) => spawn.config.image_ref.as_deref().unwrap_or(""),
            Kind::Run(_) => "",
        }
    }

    fn override_source_for_dry_run(&self) -> &str {
        match &self.request.kind {
            Kind::Spawn(spawn) => spawn.config.image_source.as_deref().unwrap_or(""),
            Kind::Run(_) => "",
        }
    }

    fn override_source_kind_for_dry_run(&self) -> &str {
        match &self.request.kind {
            Kind::Spawn(spawn) => spawn
                .config
                .image_source_kind
                .map_or("", SourceKind::as_str),
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

impl DarwinMounts {
    fn push(&mut self, mount: &RenderedMount, staging: &Staging) -> Result<(), LaunchError> {
        let source = expand_path(&mount.host);
        if !source.exists() {
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
            let host = staging.root.join(format!("dir{index}"));
            copy_dir(&source, &host)?;
            let container = format!("/mnt/wrix/dir{index}");
            self.mounts.push(RenderedMount {
                host: host.display().to_string(),
                container: container.clone(),
                mode: MountMode::Rw,
            });
            self.dir_mappings.push((container, mount.container.clone()));
            return Ok(());
        }
        let parent = source.parent().map_or_else(PathBuf::new, Path::to_path_buf);
        let file_name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let container_parent = self.file_parent_mount(&parent);
        self.file_mappings.push((
            format!("{container_parent}/{file_name}"),
            mount.container.clone(),
        ));
        Ok(())
    }

    fn file_parent_mount(&mut self, parent: &Path) -> String {
        for mount in &self.mounts {
            if mount.host == parent.display().to_string()
                && mount.container.starts_with("/mnt/wrix/file")
            {
                return mount.container.clone();
            }
        }
        let index = self
            .mounts
            .iter()
            .filter(|mount| mount.container.starts_with("/mnt/wrix/file"))
            .count();
        let container = format!("/mnt/wrix/file{index}");
        self.mounts.push(RenderedMount {
            host: parent.display().to_string(),
            container: container.clone(),
            mode: MountMode::Rw,
        });
        container
    }

    fn dir_env(&self) -> Option<String> {
        mapping_env(&self.dir_mappings)
    }

    fn file_env(&self) -> Option<String> {
        mapping_env(&self.file_mappings)
    }

    fn write_dry_run(&self, stdout: &mut impl Write) -> Result<(), LaunchError> {
        if let Some(value) = self.dir_env() {
            writeln!(stdout, "DIR_MOUNTS={value}")?;
        }
        if let Some(value) = self.file_env() {
            writeln!(stdout, "FILE_MOUNTS={value}")?;
        }
        for mount in &self.mounts {
            writeln!(stdout, "MOUNT=-v {}:{}", mount.host, mount.container)?;
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
            state.load_dolt(workspace)?;
        }
        Ok(state)
    }

    fn load_project_cache(&mut self, workspace: &Path) -> Result<(), LaunchError> {
        let output = run_service(workspace, &["service", "endpoints"])?;
        let value = serde_json::from_slice::<Value>(&output.stdout)
            .map_err(|source| LaunchError::ServiceJson { source })?;
        if value
            .pointer("/endpoints/cache_http")
            .is_none_or(Value::is_null)
        {
            return Ok(());
        }
        let host = value
            .pointer("/endpoints/cache_http/host")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let port = value
            .pointer("/endpoints/cache_http/port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .unwrap_or(0);
        let state_root = value
            .get("state_root")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let public_key_path = Path::new(state_root).join("keys/cache.pub");
        if !public_key_path.is_file() {
            return Err(LaunchError::MissingCachePublicKey {
                path: public_key_path.display().to_string(),
            });
        }
        let public_key = fs::read_to_string(&public_key_path)?.replace('\n', "");
        if !public_key.contains(':') || !public_key.ends_with('=') {
            return Err(LaunchError::InvalidCachePublicKey {
                path: public_key_path.display().to_string(),
            });
        }
        let sandbox_host = sandbox_cache_host(host)?;
        let url = format!("http://{sandbox_host}:{port}");
        self.project_cache_url = Some(url.clone());
        self.project_cache_host = Some(sandbox_host);
        self.project_cache_port = Some(port);
        self.project_cache_nix_config = Some(format!(
            "extra-substituters = {url}\nextra-trusted-public-keys = {public_key}\nbuilders-use-substitutes = true"
        ));
        Ok(())
    }

    fn load_dolt(&mut self, workspace: &Path) -> Result<(), LaunchError> {
        if Platform::CURRENT == Platform::Linux {
            let output = run_service(workspace, &["service", "dolt", "socket"])?;
            let socket = trim_stdout(&output.stdout);
            self.beads_socket = Some(configure_dolt_socket(workspace, &socket)?);
        } else {
            let port = trim_stdout(&run_service(workspace, &["service", "dolt", "port"])?.stdout)
                .parse::<u16>()
                .map_or(0, |port| port);
            let host = trim_stdout(&run_service(workspace, &["service", "dolt", "host"])?.stdout);
            self.beads_tcp = Some(BeadsTcp { host, port });
        }
        Ok(())
    }

    fn write_dry_run(&self, stdout: &mut impl Write) -> Result<(), LaunchError> {
        if let Some(url) = &self.project_cache_url {
            writeln!(stdout, "PROJECT_CACHE_URL={url}")?;
            if let Some(host) = &self.project_cache_host {
                writeln!(stdout, "ENV=WRIX_PROJECT_CACHE_HOST={host}")?;
            }
            if let Some(port) = self.project_cache_port {
                writeln!(stdout, "ENV=WRIX_PROJECT_CACHE_PORT={port}")?;
            }
            if let Some(config) = &self.project_cache_nix_config {
                writeln!(stdout, "ENV=NIX_CONFIG={config}")?;
            }
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

struct Credentials {
    deploy: PathBuf,
    signing: Option<PathBuf>,
    name: String,
}

struct PiAuth {
    mount: RenderedMount,
    container_path: String,
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
        }
    }
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
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _drop_result = fs::remove_dir_all(&self.root);
    }
}

fn install_linux_image(
    image_ref: &str,
    image_source: &str,
    kind: SourceKind,
    digest: &str,
) -> Result<(), LaunchError> {
    if image_source.is_empty() {
        return Ok(());
    }
    let desired = desired_digest(image_source, kind, digest)?;
    if !desired.is_empty()
        && run_output(
            "podman",
            &["image", "inspect", "--format", "{{.Id}}", &desired],
        )
        .is_ok_and(|output| output.status.success())
    {
        let _tag = run_output("podman", &["tag", &desired, image_ref]);
        return Ok(());
    }
    let store_ref = linux_store_ref(image_ref);
    match kind {
        SourceKind::NixDescriptor => {
            let descriptor = read_json_file(image_source)?;
            let layout = descriptor
                .get("oci_layout")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| LaunchError::MissingDescriptorLayout {
                    path: image_source.to_owned(),
                })?;
            let oci_ref = descriptor
                .get("oci_ref")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("latest");
            run_required(
                "skopeo",
                &[
                    "--insecure-policy",
                    "copy",
                    "--quiet",
                    &format!("oci:{layout}:{oci_ref}"),
                    &store_ref,
                ],
            )?;
        }
        SourceKind::DockerArchive => {
            run_required(
                "skopeo",
                &[
                    "--insecure-policy",
                    "copy",
                    "--quiet",
                    &format!("docker-archive:{image_source}"),
                    &store_ref,
                ],
            )?;
        }
    }
    if let Some(repo) = image_ref.rsplit_once(':').map(|(repo, _tag)| repo) {
        let latest = format!("{repo}:latest");
        let _tag = run_output("podman", &["tag", image_ref, &latest]);
    }
    Ok(())
}

fn install_darwin_image(
    image_ref: &str,
    image_source: &str,
    kind: SourceKind,
    digest: &str,
) -> Result<(), LaunchError> {
    if image_source.is_empty() {
        return Ok(());
    }
    if kind != SourceKind::DockerArchive {
        return Err(LaunchError::UnsupportedImageSourceKind {
            kind: kind.as_str(),
        });
    }
    let desired = desired_digest(image_source, kind, digest)?;
    if !desired.is_empty() && darwin_image_digest_present(&desired)? {
        return Ok(());
    }
    run_required("container", &["image", "load", "--input", image_source])?;
    if let Some(untagged) = darwin_first_untagged_ref(image_ref) {
        run_required("container", &["image", "tag", &untagged, image_ref])?;
    }
    Ok(())
}

fn desired_digest(
    image_source: &str,
    kind: SourceKind,
    digest: &str,
) -> Result<String, LaunchError> {
    if digest.starts_with("sha256:") {
        return Ok(digest.to_owned());
    }
    let path = Path::new(digest);
    if path.is_file() {
        let value = fs::read_to_string(path)?;
        let trimmed = value.trim();
        if trimmed.starts_with("sha256:") {
            return Ok(trimmed.to_owned());
        }
    }
    match kind {
        SourceKind::NixDescriptor => {
            let descriptor = read_json_file(image_source)?;
            descriptor
                .get("digest")
                .and_then(Value::as_str)
                .filter(|value| value.starts_with("sha256:"))
                .map(ToOwned::to_owned)
                .ok_or_else(|| LaunchError::MissingDescriptorDigest {
                    path: image_source.to_owned(),
                })
        }
        SourceKind::DockerArchive => {
            let output = run_required_output(
                "skopeo",
                &[
                    "inspect",
                    "--raw",
                    &format!("docker-archive:{image_source}"),
                ],
            )?;
            let value = serde_json::from_slice::<Value>(&output.stdout)
                .map_err(|source| LaunchError::DescriptorJson { source })?;
            value
                .pointer("/config/digest")
                .and_then(Value::as_str)
                .filter(|value| value.starts_with("sha256:"))
                .map(ToOwned::to_owned)
                .ok_or_else(|| LaunchError::MissingDockerArchiveDigest {
                    path: image_source.to_owned(),
                })
        }
    }
}

fn remember_image_ref(
    runtime: ImageRuntime,
    image_ref: &str,
    digest: &str,
) -> Result<(), LaunchError> {
    if image_ref.is_empty() {
        return Ok(());
    }
    let path = image_mru_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut records = read_image_records(&path)?;
    records.insert(
        0,
        ImageRecord {
            ref_name: image_ref.to_owned(),
            digest: digest.to_owned(),
            id: image_inspect_id(runtime, image_ref)?.unwrap_or_else(String::new),
        },
    );
    let mut seen = BTreeSet::new();
    let records = records
        .into_iter()
        .filter(|record| {
            !record.ref_name.is_empty() || !record.digest.is_empty() || !record.id.is_empty()
        })
        .filter(|record| {
            seen.insert(format!(
                "{}\0{}\0{}",
                record.ref_name, record.digest, record.id
            ))
        })
        .take(8)
        .collect::<Vec<_>>();
    let json = serde_json::to_string_pretty(&records)
        .map_err(|source| LaunchError::ImageMruJson { source })?;
    fs::write(path, format!("{json}\n"))?;
    Ok(())
}

fn prune_stale_images(
    runtime: ImageRuntime,
    image_ref: &str,
    digest: &str,
) -> Result<(), LaunchError> {
    let mut keep_refs = BTreeSet::new();
    let mut keep_ids = BTreeSet::new();
    let mut keep_digests = BTreeSet::new();
    add_normalized(&mut keep_refs, image_ref);
    add_normalized(&mut keep_digests, digest);
    if let Some(id) = image_inspect_id(runtime, image_ref)? {
        add_normalized(&mut keep_ids, &id);
    }
    if let Some(actual_digest) = image_inspect_digest(runtime, image_ref)? {
        add_normalized(&mut keep_digests, &actual_digest);
    }
    for record in read_image_records(&image_mru_path())? {
        add_normalized(&mut keep_refs, &record.ref_name);
        add_normalized(&mut keep_ids, &record.id);
        add_normalized(&mut keep_digests, &record.digest);
    }
    for image in list_images(runtime)? {
        if !image.managed && !image.legacy {
            continue;
        }
        if keep_refs.contains(&image.ref_name)
            || keep_ids.contains(&image.id)
            || keep_digests.contains(&image.digest)
        {
            continue;
        }
        if image_in_use(runtime, &image.target)? {
            continue;
        }
        delete_image(runtime, &image.target)?;
    }
    Ok(())
}

struct ListedImage {
    ref_name: String,
    target: String,
    id: String,
    digest: String,
    managed: bool,
    legacy: bool,
}

fn image_mru_path() -> PathBuf {
    if let Some(path) = env::var_os("WRIX_IMAGE_KEEP_FILE") {
        return PathBuf::from(path);
    }
    env::var_os("XDG_CACHE_HOME")
        .map_or_else(|| home_dir().join(".cache"), PathBuf::from)
        .join("wrix/image-mru.json")
}

fn read_image_records(path: &Path) -> Result<Vec<ImageRecord>, LaunchError> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    match serde_json::from_str::<Vec<ImageRecord>>(&content) {
        Ok(records) => Ok(records),
        Err(source) => {
            let mut sink = io::stderr().lock();
            writeln!(
                sink,
                "wrix: resetting invalid image MRU {}: {source}",
                path.display()
            )?;
            Ok(Vec::new())
        }
    }
}

fn list_images(runtime: ImageRuntime) -> Result<Vec<ListedImage>, LaunchError> {
    let output = match runtime {
        ImageRuntime::Podman => run_required_output(
            "podman",
            &["images", "--format", "{{.Repository}} {{.Tag}} {{.ID}}"],
        )?,
        ImageRuntime::Container => run_required_output("container", &["image", "list"])?,
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut images = Vec::new();
    for line in text.lines() {
        if let Some(image) = listed_image_from_line(runtime, line) {
            images.push(image?);
        }
    }
    Ok(images)
}

fn listed_image_from_line(
    runtime: ImageRuntime,
    line: &str,
) -> Option<Result<ListedImage, LaunchError>> {
    let mut fields = line.split_whitespace();
    let repo = fields.next()?;
    if runtime == ImageRuntime::Container && repo.eq_ignore_ascii_case("repository") {
        return None;
    }
    let tag = fields.next().unwrap_or("<none>");
    let listed_id = fields.next().unwrap_or("");
    let ref_name = if repo != "<none>" && tag != "<none>" {
        format!("{repo}:{tag}")
    } else {
        String::new()
    };
    let target = normalized_value(&ref_name)
        .or_else(|| normalized_value(listed_id))
        .unwrap_or_default();
    if target.is_empty() {
        return None;
    }
    Some(listed_image(
        runtime,
        ref_name,
        listed_id.to_owned(),
        target,
    ))
}

fn listed_image(
    runtime: ImageRuntime,
    ref_name: String,
    listed_id: String,
    target: String,
) -> Result<ListedImage, LaunchError> {
    let id = image_inspect_id(runtime, &target)?.unwrap_or(listed_id);
    let digest = image_inspect_digest(runtime, &target)?.unwrap_or_else(String::new);
    let managed = image_managed(runtime, &target)?;
    let legacy = match runtime {
        ImageRuntime::Podman => ref_name.starts_with("localhost/wrix-"),
        ImageRuntime::Container => ref_name.starts_with("wrix-"),
    };
    Ok(ListedImage {
        ref_name,
        target,
        id,
        digest,
        managed,
        legacy,
    })
}

fn image_inspect_id(runtime: ImageRuntime, target: &str) -> Result<Option<String>, LaunchError> {
    image_inspect_value(runtime, target, ImageInspectField::Id)
}

fn image_inspect_digest(
    runtime: ImageRuntime,
    target: &str,
) -> Result<Option<String>, LaunchError> {
    image_inspect_value(runtime, target, ImageInspectField::Digest)
}

#[derive(Clone, Copy)]
enum ImageInspectField {
    Id,
    Digest,
    Managed,
}

fn image_inspect_value(
    runtime: ImageRuntime,
    target: &str,
    field: ImageInspectField,
) -> Result<Option<String>, LaunchError> {
    if target.is_empty() {
        return Ok(None);
    }
    let output = match runtime {
        ImageRuntime::Podman => {
            let format = match field {
                ImageInspectField::Id => "{{.Id}}",
                ImageInspectField::Digest => "{{.Digest}}",
                ImageInspectField::Managed => "{{ index .Config.Labels \"wrix.managed\" }}",
            };
            run_output("podman", &["image", "inspect", "--format", format, target])?
        }
        ImageRuntime::Container => run_output("container", &["image", "inspect", target])?,
    };
    if !output.status.success() {
        return Ok(None);
    }
    match runtime {
        ImageRuntime::Podman => Ok(normalized_value(&trim_stdout(&output.stdout))),
        ImageRuntime::Container => inspect_container_value(&output.stdout, field),
    }
}

fn inspect_container_value(
    stdout: &[u8],
    field: ImageInspectField,
) -> Result<Option<String>, LaunchError> {
    let value = serde_json::from_slice::<Value>(stdout)
        .map_err(|source| LaunchError::DescriptorJson { source })?;
    let pointer = match field {
        ImageInspectField::Id => "/0/id",
        ImageInspectField::Digest => "/0/digest",
        ImageInspectField::Managed => "/0/labels/wrix.managed",
    };
    Ok(value
        .pointer(pointer)
        .or_else(|| match field {
            ImageInspectField::Managed => value.pointer("/0/Labels/wrix.managed"),
            _ => None,
        })
        .and_then(Value::as_str)
        .and_then(normalized_value))
}

fn image_managed(runtime: ImageRuntime, target: &str) -> Result<bool, LaunchError> {
    Ok(
        image_inspect_value(runtime, target, ImageInspectField::Managed)?.as_deref()
            == Some("true"),
    )
}

fn image_in_use(runtime: ImageRuntime, target: &str) -> Result<bool, LaunchError> {
    if runtime != ImageRuntime::Podman {
        return Ok(false);
    }
    let filter = format!("ancestor={target}");
    let output = run_output(
        "podman",
        &["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"],
    )?;
    Ok(output.status.success() && !trim_stdout(&output.stdout).is_empty())
}

fn delete_image(runtime: ImageRuntime, target: &str) -> Result<(), LaunchError> {
    let output = match runtime {
        ImageRuntime::Podman => run_output("podman", &["rmi", target])?,
        ImageRuntime::Container => run_output("container", &["image", "delete", target])?,
    };
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut sink = io::stderr().lock();
    if stderr.contains("in use") || stderr.contains("is using") {
        writeln!(
            sink,
            "prune-stale-images: {target} pinned by a container — upgrades on next start"
        )?;
    } else {
        writeln!(
            sink,
            "prune-stale-images: could not remove {target}: {stderr}"
        )?;
    }
    Ok(())
}

fn add_normalized(set: &mut BTreeSet<String>, value: &str) {
    if let Some(value) = normalized_value(value) {
        set.insert(value);
    }
}

fn normalized_value(value: &str) -> Option<String> {
    match value.trim() {
        "" | "<none>" | "<no value>" | "null" => None,
        value => Some(value.to_owned()),
    }
}

fn linux_store_ref(image_ref: &str) -> String {
    let mut store_ref = format!("containers-storage:{image_ref}");
    if let Ok(output) = run_output(
        "podman",
        &[
            "info",
            "--format",
            "{{.Store.GraphDriverName}}@{{.Store.GraphRoot}}+{{.Store.RunRoot}}",
        ],
    ) && output.status.success()
    {
        let spec = trim_stdout(&output.stdout);
        if spec.contains('@') && spec.contains('+') {
            store_ref = format!("containers-storage:[{spec}]{image_ref}");
        }
    }
    store_ref
}

fn darwin_image_digest_present(digest: &str) -> Result<bool, LaunchError> {
    let Ok(output) = run_output("container", &["image", "list"]) else {
        return Ok(false);
    };
    if !output.status.success() {
        return Ok(false);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let Some(repo) = fields.next() else {
            continue;
        };
        let Some(tag) = fields.next() else {
            continue;
        };
        let reference = format!("{repo}:{tag}");
        let inspect = run_output("container", &["image", "inspect", &reference])?;
        if !inspect.status.success() {
            continue;
        }
        let value = serde_json::from_slice::<Value>(&inspect.stdout)
            .map_err(|source| LaunchError::DescriptorJson { source })?;
        let actual = value
            .pointer("/0/digest")
            .or_else(|| value.pointer("/0/id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if actual.trim_start_matches("sha256:") == digest.trim_start_matches("sha256:") {
            let _tag = run_output("container", &["image", "tag", &reference, &reference]);
            return Ok(true);
        }
    }
    Ok(false)
}

const fn darwin_first_untagged_ref(_image_ref: &str) -> Option<String> {
    None
}

fn render_profile_mount(
    mount: &ProfileMount,
    staging: &Staging,
) -> Result<RenderedMount, LaunchError> {
    let source = expand_path(&mount.source);
    if !source.exists() {
        return Err(LaunchError::MountSourceMissing {
            path: source.display().to_string(),
        });
    }
    if source.is_dir() && mount.mode == MountMode::Ro {
        let target = staging
            .root
            .join(format!("dir{}", stable_mount_index(&source)));
        copy_dir(&source, &target)?;
        return Ok(RenderedMount {
            host: target.display().to_string(),
            container: mount.dest.clone(),
            mode: mount.mode,
        });
    }
    if file_type_is_socket(&source)? {
        return Err(LaunchError::SocketMountRejected {
            socket: source.display().to_string(),
            dest: mount.dest.clone(),
        });
    }
    Ok(RenderedMount {
        host: source.display().to_string(),
        container: mount.dest.clone(),
        mode: mount.mode,
    })
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

fn sandbox_cache_host(host: &str) -> Result<String, LaunchError> {
    let resolved = env::var("WRIX_PROJECT_CACHE_SANDBOX_HOST").unwrap_or_else(|_| {
        if Platform::CURRENT == Platform::Linux && host.starts_with("127.") {
            String::from("169.254.1.2")
        } else {
            host.to_owned()
        }
    });
    if resolved.split('.').count() == 4
        && resolved.split('.').all(|part| part.parse::<u8>().is_ok())
    {
        Ok(resolved)
    } else {
        Err(LaunchError::InvalidCacheHost { host: resolved })
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

fn run_required(program: &str, args: &[&str]) -> Result<(), LaunchError> {
    let output = run_output(program, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(LaunchError::ProcessFailed {
            program: program.to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
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

fn status_to_exit(status: std::process::ExitStatus) -> ExitCode {
    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map_or(ExitCode::FAILURE, ExitCode::from)
}

fn current_uid() -> Result<String, LaunchError> {
    let output = run_required_output("id", &["-u"])?;
    Ok(trim_stdout(&output.stdout))
}

fn read_json_file(path: &str) -> Result<Value, LaunchError> {
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).map_err(|source| LaunchError::DescriptorJson { source })
}

fn trim_stdout(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout).trim().to_owned()
}

fn non_empty_override(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

fn mapping_env(mappings: &[(String, String)]) -> Option<String> {
    (!mappings.is_empty()).then(|| {
        mappings
            .iter()
            .map(|(source, dest)| format!("{source}:{dest}"))
            .collect::<Vec<_>>()
            .join(",")
    })
}

fn first_configured_value(values: &[Option<String>]) -> Option<String> {
    values.iter().find_map(Clone::clone)
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

fn ensure_krun() -> Result<(), LaunchError> {
    if !Path::new("/dev/kvm").exists() {
        return Err(LaunchError::KvmMissing);
    }
    if run_output("krun", &["--version"]).is_ok_and(|output| output.status.success()) {
        return Ok(());
    }
    if run_output(
        "podman",
        &[
            "info",
            "--format",
            "{{range .Host.OCIRuntime.Alternatives}}{{.}}{{end}}",
        ],
    )
    .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("krun"))
    {
        return Ok(());
    }
    Err(LaunchError::KrunMissing)
}

fn deploy_key_name(workspace: &Path, configured: Option<&str>) -> String {
    if let Some(name) = configured.filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    let workspace_name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace");
    format!("{}-{}", workspace_name, host_label())
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

fn expand_path(input: &str) -> PathBuf {
    let home = env::var("HOME").unwrap_or_default();
    let user = env::var("USER").unwrap_or_default();
    let expanded = input
        .replacen('~', &home, 1)
        .replace("$HOME", &home)
        .replace("$USER", &user);
    PathBuf::from(expanded)
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

    use super::{
        DarwinMounts, HostPodmanSocket, LaunchError, NetworkMode, RenderedMount, Staging,
        deploy_key_name, linux_podman_network,
    };
    use crate::command::config::{MountMode, Platform, ProfileMount, SpawnMount};

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
        };
        let spawn_mount = SpawnMount {
            host_path: host_file.display().to_string(),
            container_path: String::from("/etc/spawn-file"),
            read_only: true,
        };
        let mut mounts = DarwinMounts::default();
        mounts
            .push(&RenderedMount::from_profile(&profile_mount), &staging)
            .unwrap();
        mounts
            .push(&RenderedMount::from_spawn(&spawn_mount), &staging)
            .unwrap();

        assert_eq!(
            mounts.dir_env().as_deref(),
            Some("/mnt/wrix/dir0:/mnt/profile-dir")
        );
        assert!(
            mounts
                .file_env()
                .is_some_and(|value| value.ends_with(":/etc/spawn-file"))
        );
        assert_eq!(mounts.mounts.len(), 2);
    }

    #[test]
    fn stable_mount_index_is_deterministic() {
        assert_eq!(
            super::stable_mount_index(&PathBuf::from("/tmp/a")),
            super::stable_mount_index(&PathBuf::from("/tmp/a"))
        );
    }

    #[test]
    fn deploy_key_name_prefers_profile_config_value() {
        assert_eq!(
            deploy_key_name(Path::new("/workspace/repo"), Some("custom-key")),
            "custom-key"
        );
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
