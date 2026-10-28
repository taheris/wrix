//! Recorded service-VM ownership and authenticated fixed-operation management.

mod apple;
mod budget;
pub mod client;
pub(crate) mod observation;
pub mod seed;
pub mod tools;
mod transport;

#[cfg(test)]
mod test;

use super::{Paths, Prepared, Record as Preparation, Reservation, check_directory, private_file};
use crate::{
    deployment,
    nix_store::{
        image::Candidate,
        native::{self, bootstrap, lease},
    },
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    num::NonZeroU16,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    process::Output,
    thread,
    time::{Duration, Instant},
};
use tools::{Program, Tools};
use wrix_core::nix_store::pool::Key;
use wrix_sandbox::image::{self, Digest, ImageId, Store};

const LEDGER: &str = "backend.json";
const COMMAND_BUDGET: Duration = Duration::from_secs(30);

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// pool host I/O failed; an interrupted mutation retains uncertain ownership: {0}
    Io(#[from] io::Error),
    /// pool host metadata is invalid: {0}
    Json(#[from] serde_json::Error),
    /// pool execution evidence failed: {0}
    Execution(#[from] wrix_state::store::Error),
    /// pool checked issuance failed: {0}
    Admission(#[from] wrix_state::nix_store::Error),
    /// pool preparation failed: {0}
    Preparation(#[from] super::Error),
    /// pool software target failed: {0}
    Software(#[from] crate::nix_store::image::Error),
    /// pool image installation failed: {0}
    Image(#[from] image::Error),
    /// pool immutable image input failed: {0}
    ImageInput(#[from] native::image::Error),
    /// pool native identity failed: {0}
    Identity(#[from] native::lease::Error),
    /// pool host operation requires its package-selected native service VM runtime
    Unsupported,
    /// pool deployment paths are invalid: {0}
    Layout(#[from] deployment::Error),
    /// pool host metadata, native identity or authenticated readiness is inconsistent
    Invalid,
    /// pool owner or native work is busy
    Busy,
    /// pool ownership is retained or uncertain; explicit reconciliation is required
    Retained,
    /// no admitted running pool owner; installed-store collection is unavailable
    Unavailable,
    /// native installed-store collection failed
    Collection,
    /// pool host helper failed with status {0:?}; no native completion is inferred
    Helper(Option<i32>),
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(try_from = "String", into = "String")]
struct NativeId(String);
impl TryFrom<String> for NativeId {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        let hex = value.strip_prefix("wrix-nix-").unwrap_or(&value);
        let length = if value.starts_with("wrix-nix-") {
            32
        } else {
            64
        };
        if hex.len() != length
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(Error::Invalid);
        }
        Ok(Self(value))
    }
}
impl From<NativeId> for String {
    fn from(value: NativeId) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Software {
    candidate: Candidate,
    digest: Digest,
    image: ImageId,
    tools: Tools,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reference: Option<image::ImageRef>,
}
impl Software {
    fn installed_image(
        image: Option<ImageId>,
        actual: Option<&Digest>,
        expected: &Digest,
    ) -> Result<ImageId, Error> {
        if actual != Some(expected) {
            return Err(Error::Invalid);
        }
        image.ok_or(Error::Invalid)
    }

    fn metadata(
        tools: &Tools,
        image: &ImageId,
        reference: &image::ImageRef,
        system: wrix_core::nix_store::pool::System,
    ) -> Result<(serde_json::Value, Option<image::ImageRef>), Error> {
        if tools.runtime() == image::Runtime::Container {
            let (config, reference) = apple::image_config(tools, image, reference, system)?;
            return Ok((config, Some(reference)));
        }
        let bytes = podman(tools, &["image", "inspect", image.as_str()])?;
        let metadata: serde_json::Value = serde_json::from_slice(&bytes)?;
        if metadata.as_array().map(Vec::len) != Some(1) {
            return Err(Error::Invalid);
        }
        Ok((
            serde_json::json!({"config":metadata[0]["Config"], "architecture":metadata[0]["Architecture"]}),
            None,
        ))
    }

    fn install(
        candidate: Candidate,
        tools: Tools,
        system: wrix_core::nix_store::pool::System,
        roots: &Path,
    ) -> Result<Self, Error> {
        if tools.platform() == tools::Platform::Apple {
            let (container, skopeo) = tools.apple_image_commands()?;
            return image::with_runtime_commands(&container, &skopeo, || {
                Self::install_native(candidate, tools, system, roots)
            });
        }
        Self::install_native(candidate, tools, system, roots)
    }

    fn install_native(
        candidate: Candidate,
        tools: Tools,
        system: wrix_core::nix_store::pool::System,
        roots: &Path,
    ) -> Result<Self, Error> {
        tools.check(system)?;
        wrix_core::private_directory::prepare_with_parents(roots).map_err(io::Error::other)?;
        tools.pin(roots)?;
        let digest = candidate.realize(roots)?;
        let mut store = image::CommandStore;
        let runtime = tools.image_runtime();
        image::install(
            &mut store,
            &image::InstallRequest::new(
                runtime,
                candidate.image(),
                candidate.source(),
                Some(&digest),
            )?,
        )?;
        let selected = image::Target::Reference(candidate.image().clone());
        let native = store.image_id(runtime, &selected)?;
        let image = match runtime {
            image::Runtime::Container => Self::installed_image(
                native,
                store.image_digest(runtime, &selected)?.as_ref(),
                &digest,
            )?,
            image::Runtime::Podman => {
                let image = native.ok_or(Error::Invalid)?;
                if !same_image(&image, &digest) {
                    return Err(Error::Invalid);
                }
                image
            }
        };
        let (metadata, reference) = Self::metadata(&tools, &image, candidate.image(), system)?;
        let architecture = match system {
            wrix_core::nix_store::pool::System::X86_64Linux => "amd64",
            wrix_core::nix_store::pool::System::Aarch64Linux => "arm64",
        };
        if metadata["config"]["Labels"]["wrix.nix-pool.bootstrap-version"] != "3"
            || metadata["config"]["Labels"]["wrix.image.kind"] != "service"
            || metadata["config"]["Labels"]["wrix.managed"] != "true"
            || metadata["architecture"] != architecture
        {
            return Err(Error::Invalid);
        }
        let environment = if tools.platform() == tools::Platform::Apple {
            Some(apple::environment(&metadata["config"]["Env"])?)
        } else {
            None
        };
        Ok(Self {
            candidate,
            digest,
            image,
            tools,
            environment,
            reference,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    #[serde(default, skip_serializing_if = "budget::Budget::is_legacy")]
    budget: budget::Budget,
    nonce: lease::Id,
    software: Software,
}
impl Attempt {
    fn name(&self) -> String {
        format!("wrix-nix-{}", self.nonce.as_str())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Target {
    attempt: Attempt,
    id: NativeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    apple: Option<apple::Binding>,
}
impl Target {
    fn check_runtime(&self) -> Result<(), Error> {
        match self.attempt.software.tools.platform() {
            tools::Platform::Podman
                if self.apple.is_none()
                    && self.id.0.len() == 64
                    && self.attempt.software.reference.is_none()
                    && self.attempt.software.environment.is_none() =>
            {
                Ok(())
            }
            tools::Platform::Apple
                if self.apple.is_some()
                    && self.id.0 == self.attempt.name()
                    && self.attempt.software.reference.is_some()
                    && self.attempt.software.environment.is_some() =>
            {
                Ok(())
            }
            _ => Err(Error::Invalid),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    target: Target,
    port: NonZeroU16,
    host_key: lease::Key,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum State {
    Starting { attempt: Attempt },
    Created { target: Target },
    Serving { connection: Connection },
    Stopping { connection: Connection },
    Released { connection: Connection },
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    deployment: deployment::Id,
    pool: Key,
    state: State,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    executions: BTreeSet<wrix_state::execution::Ticket>,
}

fn success(output: Output) -> Result<Vec<u8>, Error> {
    if !output.status.success() {
        return Err(Error::Helper(output.status.code()));
    }
    Ok(output.stdout)
}

fn command(
    tools: &Tools,
    program: Program,
    arguments: impl IntoIterator<Item = OsString>,
    budget: Option<Duration>,
) -> Result<Vec<u8>, Error> {
    let mut command = tools.command(program);
    command.args(arguments);
    success(tools.run(command, None, budget)?)
}

fn podman(tools: &Tools, arguments: &[&str]) -> Result<Vec<u8>, Error> {
    command(
        tools,
        Program::Podman,
        arguments.iter().map(OsString::from),
        Some(COMMAND_BUDGET),
    )
}

fn same_image(image: &ImageId, digest: &Digest) -> bool {
    image
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(image.as_str())
        == digest
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or(digest.as_str())
}

fn text(bytes: Vec<u8>) -> Result<String, Error> {
    Ok(String::from_utf8(bytes)
        .map_err(|_| Error::Invalid)?
        .trim()
        .to_owned())
}

fn public_key(path: &Path) -> Result<lease::Key, Error> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
        || metadata.len() > 4096
    {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    let value = String::from_utf8(bytes).map_err(|_| Error::Invalid)?;
    let mut words = value.split_whitespace();
    let algorithm = words.next().ok_or(Error::Invalid)?;
    let encoded = words.next().ok_or(Error::Invalid)?;
    Ok(format!("{algorithm} {encoded}").try_into()?)
}

#[derive(Clone, Copy)]
enum SshPath {
    KnownHosts,
    Identity,
}

fn known_hosts_option(path: &Path) -> Result<OsString, Error> {
    ssh_path_option(SshPath::KnownHosts, path)
}

fn ssh_path_option(option: SshPath, path: &Path) -> Result<OsString, Error> {
    let mut bytes = match option {
        SshPath::KnownHosts => b"UserKnownHostsFile=\"".to_vec(),
        SshPath::Identity => b"IdentityFile=\"".to_vec(),
    };
    for byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_control() {
            return Err(Error::Invalid);
        }
        if matches!(byte, b'\\' | b'\"') {
            bytes.push(b'\\');
        }
        bytes.push(*byte);
    }
    bytes.push(b'\"');
    Ok(OsString::from_vec(bytes))
}

fn released(data: &Path) -> Result<(), Error> {
    check_directory(data)?;
    let mut bytes = Vec::new();
    private_file(data, "released", false)?
        .take(3)
        .read_to_end(&mut bytes)?;
    if bytes != b"1\n" {
        return Err(Error::Retained);
    }
    match fs::symlink_metadata(data.join("resuming")) {
        Ok(_) => Err(Error::Retained),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn random() -> Result<lease::Id, Error> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let value = bytes
        .iter()
        .flat_map(|byte| {
            let alphabet = b"0123456789abcdef";
            [
                char::from(alphabet[usize::from(byte >> 4)]),
                char::from(alphabet[usize::from(byte & 15)]),
            ]
        })
        .collect::<String>();
    Ok(value.try_into()?)
}

#[derive(Deserialize)]
struct Observation {
    #[serde(rename = "Id")]
    id: NativeId,
    #[serde(rename = "Image")]
    image: ImageId,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "OCIRuntime")]
    runtime: String,
    #[serde(rename = "Config")]
    config: Configuration,
    #[serde(rename = "State")]
    state: Status,
    #[serde(rename = "Mounts")]
    mounts: Vec<Mount>,
    #[serde(rename = "HostConfig")]
    resources: Resources,
}
#[derive(Deserialize)]
struct Configuration {
    #[serde(rename = "Labels")]
    labels: BTreeMap<String, String>,
    #[serde(rename = "Annotations")]
    annotations: BTreeMap<String, String>,
}
#[derive(Deserialize)]
struct Resources {
    #[serde(rename = "Memory")]
    memory: u64,
}

#[derive(Deserialize)]
struct Status {
    #[serde(rename = "Running")]
    running: bool,
    #[serde(rename = "ExitCode")]
    exit_code: Option<i32>,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum MountKind {
    Bind,
    #[serde(other)]
    Unsupported,
}

#[derive(Deserialize)]
struct Mount {
    #[serde(rename = "Type")]
    kind: MountKind,
    #[serde(rename = "Source")]
    source: PathBuf,
    #[serde(rename = "Destination")]
    destination: PathBuf,
}

impl Record {
    fn read(root: &Path, deployment: &deployment::Id, pool: &Key) -> Result<Self, Error> {
        let file = private_file(root, LEDGER, false)?;
        if file.metadata()?.len() > 65536 {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        let record: Self = serde_json::from_slice(&bytes)?;
        if bytes.len() > 65536
            || record.version != 1
            || &record.deployment != deployment
            || &record.pool != pool
            || record.executions.len() > 64
        {
            return Err(Error::Invalid);
        }
        match &record.state {
            State::Starting { .. } => (),
            State::Created { target } => target.check_runtime()?,
            State::Serving { connection }
            | State::Stopping { connection }
            | State::Released { connection } => connection.target.check_runtime()?,
        }
        Ok(record)
    }

    fn publish(&self, root: &Path) -> Result<(), Error> {
        let mut file = tempfile::NamedTempFile::new_in(root)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.as_file().sync_all()?;
        file.persist(root.join(LEDGER))
            .map_err(|error| error.error)?;
        File::open(root)?.sync_all()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MaintenanceState {
    Serving,
    Released,
    Stopping,
}

/// A short host management window, not a worker grant; drop never stops or deletes a VM.
pub struct Owner {
    prepared: Prepared,
    record: Record,
}

impl Owner {
    pub const fn pool(&self) -> &Key {
        &self.record.pool
    }

    pub(crate) fn open(
        host: &wrix_state::host::Host,
        deployment: &deployment::Id,
        pool: &Key,
    ) -> Result<Self, Error> {
        let layout = deployment::Layout::new(
            host.roots().state().parent().ok_or(Error::Invalid)?,
            host.roots().cache().parent().ok_or(Error::Invalid)?,
        )?;
        let paths = Paths::new(&layout, pool);
        check_directory(&paths.control)?;
        check_directory(&paths.data)?;
        let ownership = private_file(&paths.control, "ownership", false)?;
        match FileExt::try_lock_exclusive(&ownership) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Err(Error::Busy),
            Err(error) => return Err(error.into()),
        }
        let preparation = Preparation::read(&paths.control)?;
        if &preparation.deployment != deployment
            || &preparation.pool != pool
            || preparation.phase != super::Phase::Prepared
        {
            return Err(Error::Retained);
        }
        let record = Record::read(&paths.control, deployment, pool)?;
        let reservation = Reservation {
            paths,
            record: preparation,
            _ownership: ownership,
        };
        Ok(Self {
            prepared: Prepared { reservation },
            record,
        })
    }

    const fn connection(&self) -> Result<&Connection, Error> {
        match &self.record.state {
            State::Serving { connection }
            | State::Stopping { connection }
            | State::Released { connection } => Ok(connection),
            _ => Err(Error::Retained),
        }
    }

    fn observe(&self, target: &Target) -> Result<Status, Error> {
        target.check_runtime()?;
        if target.attempt.software.tools.runtime() == image::Runtime::Container {
            return apple::observe(self, target);
        }
        if target.apple.is_some() {
            return Err(Error::Invalid);
        }
        let bytes = podman(&target.attempt.software.tools, &["inspect", &target.id.0])?;
        let mut observations: Vec<Observation> = serde_json::from_slice(&bytes)?;
        if observations.len() != 1 {
            return Err(Error::Invalid);
        }
        let observation = observations.pop().ok_or(Error::Invalid)?;
        if observation.id != target.id
            || !same_image(&observation.image, &target.attempt.software.digest)
            || observation.name.trim_start_matches('/') != target.attempt.name()
            || Path::new(&observation.runtime) != target.attempt.software.tools.vm()
            || !target.attempt.budget.matches(
                observation.resources.memory,
                &observation.config.annotations,
            )
        {
            return Err(Error::Invalid);
        }
        for (key, value) in [
            ("wrix.managed", "true".to_owned()),
            ("wrix.kind", "nix-pool".to_owned()),
            ("wrix.deployment", self.record.deployment.to_string()),
            (
                "wrix.nix.repository",
                self.record.pool.repository.to_string(),
            ),
            (
                "wrix.nix.system",
                self.record.pool.system.as_str().to_owned(),
            ),
            ("wrix.nix.attempt", target.attempt.nonce.as_str().to_owned()),
        ] {
            if observation.config.labels.get(key) != Some(&value) {
                return Err(Error::Invalid);
            }
        }
        let mut data = 0;
        for mount in &observation.mounts {
            if !matches!(mount.kind, MountKind::Bind) {
                return Err(Error::Invalid);
            }
            if mount.destination == Path::new("/var/lib/wrix/nix-pool") {
                if mount.source != self.prepared.data_root() {
                    return Err(Error::Invalid);
                }
                data += 1;
            } else if !["/etc/hosts", "/etc/hostname", "/etc/resolv.conf"]
                .iter()
                .any(|path| mount.destination == Path::new(path))
            {
                return Err(Error::Invalid);
            }
        }
        if data != 1 {
            return Err(Error::Invalid);
        }
        Ok(observation.state)
    }

    fn rpc(
        &self,
        connection: &Connection,
        operation: &native::Operation,
        budget: Option<Duration>,
    ) -> Result<native::Reply, Error> {
        if connection.target.apple.is_some() {
            apple::validate_binding(self, &connection.target, connection.port)?;
        }
        self.observe(&connection.target).and_then(|observation| {
            if observation.running {
                Ok(())
            } else {
                Err(Error::Retained)
            }
        })?;
        let root = &self.prepared.reservation.paths.control;
        private_file(root, "admin", false)?;
        private_file(root, "known-hosts", false)?;
        let mut command = connection
            .target
            .attempt
            .software
            .tools
            .command(Program::Ssh);
        let known_hosts = known_hosts_option(&root.join("known-hosts"))?;
        command
            .args([
                "-F",
                "/dev/null",
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "IdentityAgent=none",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "ConnectTimeout=2",
                "-o",
                "ConnectionAttempts=1",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
            ])
            .arg("-o")
            .arg(known_hosts)
            .arg("-p")
            .arg(connection.port.to_string())
            .arg("-i")
            .arg(root.join("admin"))
            .args(["root@127.0.0.1", "control"]);
        let input = serde_json::to_vec(operation)?;
        let output = connection
            .target
            .attempt
            .software
            .tools
            .run(command, Some(input), budget)?;
        Ok(serde_json::from_slice(&success(output)?)?)
    }

    pub fn runtime_identity(&self) -> Result<&str, Error> {
        Ok(&self.connection()?.target.id.0)
    }

    pub(crate) fn ensure_ready(&mut self) -> Result<(), Error> {
        if self.connection()?.target.attempt.software.tools.runtime() != image::Runtime::Container
            && self.connection()?.target.attempt.budget != budget::Budget::HostIoPasst
        {
            // Retained older transports may be reconciled and drained, never issued to new clients.
            return Err(Error::Retained);
        }
        if matches!(self.record.state, State::Released { .. }) {
            self.resume()?;
        }
        match self.inspect()? {
            native::Reply::Ready { version: 1, .. } => Ok(()),
            native::Reply::Busy => Err(Error::Busy),
            _ => Err(Error::Retained),
        }
    }

    pub fn inspect(&self) -> Result<native::Reply, Error> {
        if !matches!(self.record.state, State::Serving { .. }) {
            return Err(Error::Retained);
        }
        let connection = self.connection()?;
        if connection.target.attempt.software.tools.runtime() != image::Runtime::Container
            && connection.target.attempt.budget != budget::Budget::HostIoPasst
        {
            return Err(Error::Retained);
        }
        self.rpc(
            connection,
            &native::Operation::Inspect {},
            Some(Duration::from_secs(5)),
        )
    }

    /// Authenticated native activity observation; an idle observation alone is never a release capability.
    pub fn activity(&self) -> Result<native::Activity, Error> {
        self.inspect()?;
        match self.rpc(
            self.connection()?,
            &native::Operation::InspectWork {},
            Some(COMMAND_BUDGET),
        )? {
            native::Reply::Work { activity } => Ok(activity),
            native::Reply::Busy => Err(Error::Busy),
            _ => Err(Error::Retained),
        }
    }

    pub fn collect(&self) -> Result<(), Error> {
        if client::protected(
            &self.prepared.reservation.paths.control,
            self.pool(),
            &self.record.executions,
        )? {
            return Err(Error::Busy);
        }
        if !matches!(self.record.state, State::Serving { .. }) {
            return Err(Error::Retained);
        }
        match self.rpc(self.connection()?, &native::Operation::Collect {}, None)? {
            native::Reply::Applied => Ok(()),
            native::Reply::Busy => Err(Error::Busy),
            native::Reply::Failed => Err(Error::Collection),
            _ => Err(Error::Retained),
        }
    }

    /// Complete census preflight only; it neither stops nor starts native work.
    pub(crate) fn maintenance_state(&self, recovering: bool) -> Result<MaintenanceState, Error> {
        if client::protected(
            &self.prepared.reservation.paths.control,
            self.pool(),
            &self.record.executions,
        )? {
            return Err(Error::Busy);
        }
        match &self.record.state {
            State::Serving { .. } => {
                if !matches!(
                    self.inspect()?,
                    native::Reply::Ready {
                        version: 1,
                        leases: 0
                    }
                ) || self.activity()? != native::Activity::Idle
                {
                    return Err(Error::Busy);
                }
                Ok(MaintenanceState::Serving)
            }
            State::Released { connection } => {
                released(self.prepared.data_root())?;
                if !self.absent(connection)? {
                    return Err(Error::Retained);
                }
                Ok(MaintenanceState::Released)
            }
            State::Stopping { connection } if recovering => {
                if self.absent(connection)? {
                    released(self.prepared.data_root())?;
                } else {
                    let observation = self.observe(&connection.target)?;
                    if !observation.running {
                        if observation.exit_code != Some(0) {
                            return Err(Error::Retained);
                        }
                        released(self.prepared.data_root())?;
                    }
                }
                Ok(MaintenanceState::Stopping)
            }
            _ => Err(Error::Retained),
        }
    }

    fn absent(&self, connection: &Connection) -> Result<bool, Error> {
        connection.target.check_runtime()?;
        if connection.target.attempt.software.tools.runtime() == image::Runtime::Container {
            apple::validate_binding(self, &connection.target, connection.port)?;
            let absent = apple::absent(&connection.target)?;
            if absent {
                apple::clean_exit(self, &connection.target)?;
            }
            return Ok(absent);
        }
        let tools = &connection.target.attempt.software.tools;
        let mut command = tools.command(Program::Podman);
        command.args(["container", "exists", &connection.target.id.0]);
        let output = tools.run(command, None, Some(COMMAND_BUDGET))?;
        match (output.status.code(), output.stdout.is_empty()) {
            (Some(1), true) => Ok(true),
            (Some(0), true) => Ok(false),
            _ => Err(Error::Retained),
        }
    }

    pub fn shutdown(&mut self) -> Result<(), Error> {
        if client::protected(
            &self.prepared.reservation.paths.control,
            self.pool(),
            &self.record.executions,
        )? {
            return Err(Error::Busy);
        }
        if !matches!(
            self.record.state,
            State::Serving { .. } | State::Stopping { .. } | State::Released { .. }
        ) {
            return Err(Error::Retained);
        }
        let connection = self.connection()?.clone();
        if matches!(self.record.state, State::Released { .. }) {
            released(self.prepared.data_root())?;
            return if self.absent(&connection)? {
                Ok(())
            } else {
                Err(Error::Retained)
            };
        }
        self.record.state = State::Stopping {
            connection: connection.clone(),
        };
        self.record
            .publish(&self.prepared.reservation.paths.control)?;
        if self.absent(&connection)? {
            // An interrupted stop needs both the independent guest release certificate and exact runtime absence.
            released(self.prepared.data_root())?;
        } else {
            let observation = self.observe(&connection.target)?;
            if observation.running {
                match self.rpc(
                    &connection,
                    &native::Operation::Shutdown {},
                    Some(COMMAND_BUDGET),
                ) {
                    Ok(native::Reply::Busy) => {
                        self.record.state = State::Serving { connection };
                        self.record
                            .publish(&self.prepared.reservation.paths.control)?;
                        return Err(Error::Busy);
                    }
                    Ok(native::Reply::Stopping) | Err(Error::Helper(_) | Error::Json(_)) => (),
                    Ok(_) => return Err(Error::Retained),
                    Err(error) => return Err(error),
                }
                if connection.target.apple.is_some() {
                    apple::wait(self, &connection.target)?;
                } else {
                    let exit = text(podman(
                        &connection.target.attempt.software.tools,
                        &["wait", &connection.target.id.0],
                    )?)?;
                    if exit != "0" {
                        return Err(Error::Retained);
                    }
                }
            }
            let observation = self.observe(&connection.target)?;
            if observation.running || observation.exit_code != Some(0) {
                return Err(Error::Retained);
            }
            released(self.prepared.data_root())?;
            if connection.target.apple.is_some() {
                apple::remove(self, &connection.target)?;
            } else {
                let removed = text(podman(
                    &connection.target.attempt.software.tools,
                    &["rm", &connection.target.id.0],
                )?)?;
                if removed != connection.target.id.0 {
                    return Err(Error::Retained);
                }
            }
            if !self.absent(&connection)? {
                return Err(Error::Retained);
            }
        }
        self.record.state = State::Released { connection };
        self.record
            .publish(&self.prepared.reservation.paths.control)?;
        Ok(())
    }
}

impl Prepared {
    /// Install the recorded service image and start one native VM; failures retain intent, never force cleanup.
    pub(crate) fn start(self, candidate: Candidate, tools: Tools) -> Result<Owner, Error> {
        tools.check(self.pool().system)?;
        match fs::symlink_metadata(self.reservation.paths.control.join(LEDGER)) {
            Ok(_) => return Err(Error::Retained),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        private_file(&self.reservation.paths.control, "admin", false)?;
        for entry in fs::read_dir(self.data_root())? {
            if !matches!(
                entry?.file_name().to_str(),
                Some("bootstrap.json" | "store.ext4")
            ) {
                return Err(Error::Invalid);
            }
        }
        let bootstrap: bootstrap::Configuration =
            serde_json::from_reader(private_file(self.data_root(), "bootstrap.json", false)?)?;
        if bootstrap.storage() != bootstrap::Storage::Preallocated {
            return Err(Error::Invalid);
        }
        let mut keygen = tools.command(Program::Keygen);
        keygen
            .args(["-y", "-f"])
            .arg(self.reservation.paths.control.join("admin"));
        let actual = lease::Key::try_from(text(success(tools.run(
            keygen,
            None,
            Some(COMMAND_BUDGET),
        )?)?)?)?;
        if &actual != bootstrap.admin_key() {
            return Err(Error::Invalid);
        }
        private_file(self.data_root(), "store.ext4", false)?;
        let software = Software::install(
            candidate,
            tools,
            self.pool().system,
            &self.reservation.paths.control.join("software-roots"),
        )?;
        let nonce = random()?;
        let attempt = Attempt {
            budget: budget::Budget::HostIoPasst,
            nonce,
            software,
        };
        let record = Record {
            version: 1,
            deployment: self.reservation.record.deployment.clone(),
            pool: self.pool().clone(),
            state: State::Starting {
                attempt: attempt.clone(),
            },
            executions: BTreeSet::new(),
        };
        let mut owner = Owner {
            prepared: self,
            record,
        };
        owner
            .record
            .publish(&owner.prepared.reservation.paths.control)?;
        owner.boot(&attempt, None)?;
        Ok(owner)
    }
}

impl Reservation {
    /// Generate management authority outside service data; only its public key is a VM input.
    pub(crate) fn prepare(self, tools: &Tools) -> Result<Prepared, Error> {
        tools.check(self.pool().system)?;
        let root = &self.paths.control;
        let mut command = tools.command(Program::Keygen);
        command
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "", "-f"])
            .arg(root.join("admin"));
        success(tools.run(command, None, Some(COMMAND_BUDGET))?)?;
        let key = public_key(&root.join("admin.pub"))?;
        File::open(root)?.sync_all()?;
        self.allocate(key).map_err(Into::into)
    }
}

impl Owner {
    fn boot(&mut self, attempt: &Attempt, expected_key: Option<&lease::Key>) -> Result<(), Error> {
        if attempt.software.tools.runtime() == image::Runtime::Container {
            let (target, port) = apple::boot(self, attempt)?;
            return self.ready(target, port, expected_key);
        }
        let tools = &attempt.software.tools;
        let name = attempt.name();
        let mut arguments = ["create", "--name", name.as_str(), "--runtime"]
            .map(OsString::from)
            .to_vec();
        arguments.push(tools.vm().into_os_string());
        arguments.extend(attempt.budget.arguments().iter().map(OsString::from));
        arguments.extend(
            [
                "--cpus=2",
                "--pids-limit=256",
                "--log-opt=max-size=16m",
                "--publish=127.0.0.1::2222",
            ]
            .map(OsString::from),
        );
        for (key, value) in [
            ("wrix.managed", "true".to_owned()),
            ("wrix.kind", "nix-pool".to_owned()),
            ("wrix.deployment", self.record.deployment.to_string()),
            (
                "wrix.nix.repository",
                self.record.pool.repository.to_string(),
            ),
            (
                "wrix.nix.system",
                self.record.pool.system.as_str().to_owned(),
            ),
            ("wrix.nix.attempt", attempt.nonce.as_str().to_owned()),
        ] {
            arguments.push("--label".into());
            arguments.push(format!("{key}={value}").into());
        }
        arguments.push("--volume".into());
        let mut mount = self.prepared.data_root().as_os_str().to_os_string();
        mount.push(":/var/lib/wrix/nix-pool:rw");
        arguments.push(mount);
        arguments.push(attempt.software.image.as_str().into());
        let id: NativeId = text(command(
            tools,
            Program::Podman,
            arguments,
            Some(COMMAND_BUDGET),
        )?)?
        .try_into()?;
        let target = Target {
            attempt: attempt.clone(),
            id,
            apple: None,
        };
        self.record.state = State::Created {
            target: target.clone(),
        };
        self.record
            .publish(&self.prepared.reservation.paths.control)?;
        self.observe(&target)?;
        podman(tools, &["start", &target.id.0])?;
        let endpoint = text(podman(tools, &["port", &target.id.0, "2222/tcp"])?)?;
        let port: NonZeroU16 = endpoint
            .strip_prefix("127.0.0.1:")
            .ok_or(Error::Invalid)?
            .parse()
            .map_err(|_| Error::Invalid)?;
        self.ready(target, port, expected_key)
    }

    fn ready(
        &mut self,
        target: Target,
        port: NonZeroU16,
        expected_key: Option<&lease::Key>,
    ) -> Result<(), Error> {
        let deadline = Instant::now() + Duration::from_secs(60);
        let host_key = loop {
            if self
                .prepared
                .data_root()
                .join("host-key.pub")
                .try_exists()?
            {
                break public_key(&self.prepared.data_root().join("host-key.pub"))?;
            }
            if Instant::now() >= deadline || !self.observe(&target)?.running {
                return Err(Error::Retained);
            }
            thread::sleep(Duration::from_millis(50));
        };
        if expected_key.is_some_and(|key| key != &host_key) {
            return Err(Error::Invalid);
        }
        let connection = Connection {
            target,
            port,
            host_key,
        };
        let root = &self.prepared.reservation.paths.control;
        let mut known = tempfile::NamedTempFile::new_in(root)?;
        writeln!(
            known,
            "[127.0.0.1]:{} {}",
            connection.port,
            connection.host_key.as_str()
        )?;
        known.as_file().sync_all()?;
        known
            .persist(root.join("known-hosts"))
            .map_err(|error| error.error)?;
        File::open(root)?.sync_all()?;
        loop {
            match self.rpc(
                &connection,
                &native::Operation::Inspect {},
                Some(Duration::from_secs(5)),
            ) {
                Ok(native::Reply::Ready {
                    version: 1,
                    leases: 0,
                }) => break,
                Ok(native::Reply::Busy) | Err(Error::Helper(_) | Error::Io(_))
                    if Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(50));
                }
                Ok(_) => return Err(Error::Invalid),
                Err(error) => return Err(error),
            }
        }
        self.record.state = State::Serving { connection };
        self.record.publish(root)?;
        Ok(())
    }

    pub(crate) fn selected(&self, candidate: &Candidate, tools: &Tools) -> Result<bool, Error> {
        let software = &self.connection()?.target.attempt.software;
        Ok(software.candidate == *candidate && software.tools == *tools)
    }

    /// Explicit host update only, after the complete pool census has drained.
    /// Failed candidate acquisition retains the old software roots and released owner.
    pub(crate) fn update(&mut self, candidate: &Candidate, tools: &Tools) -> Result<(), Error> {
        let State::Released { connection } = &self.record.state else {
            return Err(Error::Retained);
        };
        let old = connection.clone();
        released(self.prepared.data_root())?;
        if old.target.apple.is_some() {
            apple::clean_exit(self, &old.target)?;
        }
        if !self.absent(&old)? {
            return Err(Error::Retained);
        }
        // Never overwrite the old owner's roots: a failed candidate is not rollback evidence.
        let nonce = random()?;
        let roots = self
            .prepared
            .reservation
            .paths
            .control
            .join("software-roots/updates")
            .join(nonce.as_str());
        let software =
            Software::install(candidate.clone(), tools.clone(), self.pool().system, &roots)?;
        let (metadata, _) = Software::metadata(
            &software.tools,
            &software.image,
            software
                .reference
                .as_ref()
                .unwrap_or_else(|| software.candidate.image()),
            self.pool().system,
        )?;
        if metadata["config"]["Labels"]["wrix.nix-pool.upgrade-version"] != "1" {
            return Err(Error::Invalid);
        }
        let mut previous = Vec::new();
        private_file(
            &self.prepared.data_root().join("owner-active"),
            "software",
            false,
        )?
        .take(4097)
        .read_to_end(&mut previous)?;
        if previous.len() > 4096 {
            return Err(Error::Invalid);
        }
        let previous = String::from_utf8(previous)
            .map_err(|_| Error::Invalid)?
            .try_into()?;
        let configuration = bootstrap::Configuration::upgrade(
            self.resume_configuration()?.admin_key().clone(),
            previous,
        );
        let attempt = Attempt {
            budget: old.target.attempt.budget,
            nonce,
            software,
        };
        // Persist new ownership intent before changing bootstrap inputs or touching native state.
        self.record.state = State::Starting {
            attempt: attempt.clone(),
        };
        self.record
            .publish(&self.prepared.reservation.paths.control)?;
        self.publish_resume_configuration(&configuration)?;
        self.boot(&attempt, Some(&old.host_key))
    }

    fn resume_configuration(&self) -> Result<bootstrap::Configuration, Error> {
        let configuration: bootstrap::Configuration = serde_json::from_reader(private_file(
            &self.prepared.reservation.paths.data,
            "bootstrap.json",
            false,
        )?)?;
        Ok(bootstrap::Configuration::resume(
            configuration.admin_key().clone(),
        ))
    }

    fn publish_resume_configuration(
        &self,
        configuration: &bootstrap::Configuration,
    ) -> Result<(), Error> {
        let mut file = tempfile::NamedTempFile::new_in(self.prepared.data_root())?;
        file.write_all(&serde_json::to_vec(configuration)?)?;
        file.as_file().sync_all()?;
        file.persist(self.prepared.data_root().join("bootstrap.json"))
            .map_err(|error| error.error)?;
        File::open(self.prepared.data_root())?.sync_all()?;
        Ok(())
    }

    /// Resume only the recorded software after prior native release and runtime removal.
    pub fn resume(&mut self) -> Result<(), Error> {
        let State::Released { connection } = &self.record.state else {
            return Err(Error::Retained);
        };
        let tools = &connection.target.attempt.software.tools;
        if tools.platform() == tools::Platform::Apple {
            released(self.prepared.data_root())?;
            apple::clean_exit(self, &connection.target)?;
            let (container, skopeo) = tools.apple_image_commands()?;
            return image::with_runtime_commands(&container, &skopeo, || self.resume_native());
        }
        self.resume_native()
    }

    fn resume_native(&mut self) -> Result<(), Error> {
        let State::Released { connection } = &self.record.state else {
            return Err(Error::Retained);
        };
        let old = connection.clone();
        released(self.prepared.data_root())?;
        if old.target.apple.is_some() {
            apple::clean_exit(self, &old.target)?;
        }
        old.target
            .attempt
            .software
            .tools
            .check(self.pool().system)?;
        if !self.absent(&old)? {
            return Err(Error::Retained);
        }
        let software = &old.target.attempt.software;
        let digest = software.candidate.realize(
            &self
                .prepared
                .reservation
                .paths
                .control
                .join("software-roots"),
        )?;
        if digest != software.digest {
            return Err(Error::Invalid);
        }
        let mut store = image::CommandStore;
        image::install(
            &mut store,
            &image::InstallRequest::new(
                software.tools.runtime(),
                software.candidate.image(),
                software.candidate.source(),
                Some(&digest),
            )?,
        )?;
        if store.image_id(
            software.tools.runtime(),
            &image::Target::Reference(software.candidate.image().clone()),
        )? != Some(software.image.clone())
        {
            return Err(Error::Invalid);
        }
        let configuration = self.resume_configuration()?;
        let nonce = random()?;
        let attempt = Attempt {
            budget: old.target.attempt.budget,
            nonce,
            software: old.target.attempt.software,
        };
        self.record.state = State::Starting {
            attempt: attempt.clone(),
        };
        self.record
            .publish(&self.prepared.reservation.paths.control)?;
        self.publish_resume_configuration(&configuration)?;
        self.boot(&attempt, Some(&old.host_key))
    }
}
