//! Fixed guest-side Nix and read-only filesystem sessions with durable pool protection.

pub mod bootstrap;
pub mod image;
pub mod lease;
mod restart;

#[cfg(test)]
mod test;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::Mutex,
    time::timeout,
};

const DATA: &str = "/var/lib/wrix/nix-pool";
const RUN: &str = "/run/wrix-nix-pool";
const CONFIG: &str = "/etc/wrix/nix-pool-native.json";
const MAX_MESSAGE: u64 = 1_048_576;
const IO_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// installed-store guest operation requires its service VM role
    Role,
    /// installed-store guest input or retained state is invalid
    Invalid,
    /// installed-store guest request is unauthorized
    Unauthorized,
    /// installed-store guest operation exceeded its I/O budget
    Timeout,
    /// installed-store native work or readers have not drained
    Busy,
    /// installed-store guest operation failed: {0}
    Io(#[from] io::Error),
    /// installed-store guest message is invalid: {0}
    Json(#[from] serde_json::Error),
    /// installed-store guest identity is invalid: {0}
    Lease(#[from] lease::Error),
    /// installed-store image input is invalid: {0}
    Image(#[from] image::Error),
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Client,
    Export,
}

impl Role {
    const fn uid(self) -> u32 {
        match self {
            Self::Client => 2000,
            Self::Export => 2001,
        }
    }
    const fn name(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Export => "export",
        }
    }
    fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "client" | "nix-client" => Ok(Self::Client),
            "export" | "nix-export" => Ok(Self::Export),
            _ => Err(Error::Unauthorized),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Inspect {},
    Register {
        id: lease::Id,
        key: lease::Key,
    },
    Release {
        id: lease::Id,
    },
    Collect {},
    Shutdown {},
    Seed {
        image: image::Id,
        paths: Vec<image::StorePath>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Ready { version: u16, leases: usize },
    Applied,
    Busy,
    Rejected,
    Failed,
    Stopping,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    id: lease::Id,
    key: lease::Key,
    closing: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    nix_daemon: PathBuf,
    nix_store: PathBuf,
    sftp_server: PathBuf,
    helper: PathBuf,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Admission {
    version: u16,
    id: lease::Id,
    role: Role,
}

enum AdmissionState {
    Serving,
    Stopping,
}

struct State {
    admission: AdmissionState,
    leases: BTreeMap<lease::Id, Record>,
    gc: File,
    configuration: Configuration,
}

fn role_root() -> Result<(), Error> {
    if !cfg!(target_os = "linux")
        || rustix::process::geteuid().as_raw() != 0
        || fs::read_to_string("/etc/wrix/nix-pool-service")? != "1\n"
        || Path::new("/etc/wrix/image-agent").try_exists()?
    {
        return Err(Error::Role);
    }
    Ok(())
}

fn private_file(path: &Path) -> Result<Vec<u8>, Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.len() > MAX_MESSAGE
    {
        return Err(Error::Invalid);
    }
    Ok(fs::read(path)?)
}

fn configuration() -> Result<Configuration, Error> {
    let config: Configuration = serde_json::from_slice(&fs::read(CONFIG)?)?;
    for (path, suffix) in [
        (&config.nix_daemon, "/bin/nix-daemon"),
        (&config.nix_store, "/bin/nix-store"),
        (&config.sftp_server, "/libexec/sftp-server"),
        (&config.helper, "/bin/wrix-nix-pool-owner"),
    ] {
        let text = path.to_str().ok_or(Error::Invalid)?;
        if !text.starts_with("/nix/store/")
            || !text.ends_with(suffix)
            || text.bytes().any(|byte| byte.is_ascii_control())
            || !path.is_file()
        {
            return Err(Error::Invalid);
        }
    }
    Ok(config)
}

fn records() -> Result<BTreeMap<lease::Id, Record>, Error> {
    let mut records = BTreeMap::new();
    for entry in fs::read_dir(Path::new(DATA).join("leases"))? {
        let entry = entry?;
        let record: Record = serde_json::from_slice(&private_file(&entry.path())?)?;
        if record.version != 1
            || entry.file_name() != std::ffi::OsStr::new(record.id.as_str())
            || records.insert(record.id.clone(), record).is_some()
        {
            return Err(Error::Invalid);
        }
    }
    Ok(records)
}

fn commit(record: &Record) -> Result<(), Error> {
    let directory = Path::new(DATA).join("leases");
    let mut file = tempfile::NamedTempFile::new_in(&directory)?;
    serde_json::to_writer(file.as_file_mut(), record)?;
    file.as_file().sync_all()?;
    file.persist(directory.join(record.id.as_str()))
        .map_err(|error| Error::Io(error.error))?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn daemon_pid() -> Result<u32, Error> {
    let pid: u32 = fs::read_to_string(Path::new(RUN).join("daemon.pid"))?
        .trim()
        .parse()
        .map_err(|_| Error::Invalid)?;
    if pid < 2 {
        return Err(Error::Invalid);
    }
    Ok(pid)
}

fn quiescent() -> Result<bool, Error> {
    let pid = daemon_pid()?;
    let procs = fs::read_to_string("/sys/fs/cgroup/wrix-nix-pool/work/nix-daemon/cgroup.procs")?;
    let parsed = procs
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| Error::Invalid)?;
    if parsed != [pid] {
        return Ok(false);
    }
    for entry in fs::read_dir("/sys/fs/cgroup/wrix-nix-pool/work")? {
        let entry = entry?;
        if entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(b"nix-build-")
        {
            let events = fs::read_to_string(entry.path().join("cgroup.events"))?;
            if !events.lines().any(|line| line == "populated 0") {
                return Ok(false);
            }
        }
    }
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        if !entry
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let status = match fs::read_to_string(entry.path().join("status")) {
            Ok(status) => status,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for line in status.lines().filter(|line| line.starts_with("Uid:")) {
            let uid: u32 = line
                .split_whitespace()
                .nth(1)
                .ok_or(Error::Invalid)?
                .parse()
                .map_err(|_| Error::Invalid)?;
            if matches!(uid, 2000 | 2001) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

async fn receive<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T, Error> {
    timeout(IO_WAIT, receive_frame(stream))
        .await
        .map_err(|_| Error::Timeout)?
}

async fn receive_frame<T: serde::de::DeserializeOwned>(
    stream: &mut UnixStream,
) -> Result<T, Error> {
    let mut bytes = Vec::new();
    let mut reader = BufReader::new(stream.take(MAX_MESSAGE + 1));
    reader.read_until(b'\n', &mut bytes).await?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_MESSAGE || bytes.last() != Some(&b'\n') {
        return Err(Error::Invalid);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

async fn send<T: Serialize + Sync>(stream: &mut UnixStream, value: &T) -> Result<(), Error> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() as u64 >= MAX_MESSAGE {
        return Err(Error::Invalid);
    }
    bytes.push(b'\n');
    timeout(IO_WAIT, stream.write_all(&bytes))
        .await
        .map_err(|_| Error::Timeout)??;
    Ok(())
}

fn command(program: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
        .env_clear()
        .env("HOME", "/var/empty")
        .env("NIX_CONF_DIR", "/etc/nix")
        .env("NIX_USER_CONF_FILES", "/dev/null")
        .env(
            "NIX_DAEMON_SOCKET_PATH",
            "/nix/var/nix/daemon-socket/socket",
        );
    command
}

async fn seed(
    state: &State,
    image: &image::Id,
    paths: &[image::StorePath],
) -> Result<Reply, Error> {
    use std::collections::BTreeSet;
    if paths.is_empty() || paths.len() > 4096 {
        return Err(Error::Invalid);
    }
    let expected: BTreeSet<_> = paths.iter().cloned().collect();
    if expected.len() != paths.len() {
        return Err(Error::Invalid);
    }
    let archive = Path::new(DATA)
        .join("incoming")
        .join(format!("{}.nar", image.as_str()));
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&archive)?;
    let metadata = input.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.len() > 8_589_934_592
    {
        return Err(Error::Invalid);
    }
    let output = tempfile::NamedTempFile::new_in("/nix/var/nix")?;
    let status = command(&state.configuration.nix_store)
        .env("NIX_REMOTE", "daemon")
        .arg("--import")
        .stdin(input)
        .stdout(output.as_file().try_clone()?)
        .status()
        .await?;
    if !status.success() {
        return Ok(Reply::Failed);
    }
    if output.as_file().metadata()?.len() > 2_097_152 {
        return Err(Error::Invalid);
    }
    let text = fs::read_to_string(output.path())?;
    let actual = text
        .lines()
        .map(|line| image::StorePath::try_from(line.to_owned()))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if actual != expected {
        return Err(Error::Invalid);
    }
    let directory = Path::new("/nix/var/nix/gcroots/images").join(image.as_str());
    fs::create_dir_all(&directory)?;
    for path in paths {
        let root = directory.join(path.basename());
        match fs::read_link(&root) {
            Ok(target) if target == Path::new(path.as_str()) => (),
            Ok(_) => return Err(Error::Invalid),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::os::unix::fs::symlink(path.as_str(), root)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    File::open(directory)?.sync_all()?;
    Ok(Reply::Applied)
}

#[cfg(target_os = "linux")]
fn signal_shutdown() -> Result<(), Error> {
    let pid =
        rustix::process::Pid::from_raw(i32::try_from(daemon_pid()?).map_err(|_| Error::Invalid)?)
            .ok_or(Error::Invalid)?;
    let handle = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
        .map_err(io::Error::from)?;
    if !quiescent()? {
        return Err(Error::Busy);
    }
    rustix::process::pidfd_send_signal(handle, rustix::process::Signal::TERM)
        .map_err(io::Error::from)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn signal_shutdown() -> Result<(), Error> {
    Err(Error::Role)
}

async fn acquire<'a, T: Send>(
    state: &'a Mutex<T>,
    operation: &Operation,
) -> Result<tokio::sync::MutexGuard<'a, T>, Error> {
    if matches!(
        operation,
        Operation::Inspect {} | Operation::Collect {} | Operation::Shutdown {}
    ) {
        state.try_lock().map_err(|_| Error::Busy)
    } else {
        Ok(state.lock().await)
    }
}

async fn administer(stream: &mut UnixStream, state: &Mutex<State>) -> Result<(), Error> {
    if stream.peer_cred()?.uid() != 0 {
        return Err(Error::Unauthorized);
    }
    let operation: Operation = receive(stream).await?;
    let mut state = match acquire(state, &operation).await {
        Ok(state) => state,
        Err(Error::Busy) => return send(stream, &Reply::Busy).await,
        Err(error) => return Err(error),
    };
    FileExt::try_lock_shared(&state.gc)?;
    if matches!(state.admission, AdmissionState::Stopping) {
        return send(stream, &Reply::Stopping).await;
    }
    let reply = match operation {
        Operation::Inspect {} => Reply::Ready {
            version: 1,
            leases: state.leases.len(),
        },
        Operation::Register { id, key } => match state.leases.get(&id) {
            Some(record) if record.key == key && !record.closing => Reply::Applied,
            Some(_) => Reply::Rejected,
            None if state.leases.len() >= 64
                || state.leases.values().any(|record| record.key == key) =>
            {
                Reply::Rejected
            }
            None => {
                let record = Record {
                    version: 1,
                    id: id.clone(),
                    key,
                    closing: false,
                };
                commit(&record)?;
                state.leases.insert(id, record);
                Reply::Applied
            }
        },
        Operation::Release { id } => {
            if let Some(record) = state.leases.get_mut(&id) {
                record.closing = true;
                commit(record)?;
                if quiescent()? {
                    fs::remove_file(Path::new(DATA).join("leases").join(id.as_str()))?;
                    File::open(Path::new(DATA).join("leases"))?.sync_all()?;
                    state.leases.remove(&id);
                    Reply::Applied
                } else {
                    Reply::Busy
                }
            } else {
                Reply::Rejected
            }
        }
        Operation::Shutdown {} => {
            if !state.leases.is_empty() || !quiescent()? {
                Reply::Busy
            } else {
                state.admission = AdmissionState::Stopping;
                let marker = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(Path::new(RUN).join("stopping"))?;
                marker.sync_all()?;
                let response = send(stream, &Reply::Stopping).await;
                signal_shutdown()?;
                return response;
            }
        }
        Operation::Seed { image, paths } => seed(&state, &image, &paths).await?,
        Operation::Collect {} => {
            if !state.leases.is_empty() || !quiescent()? {
                Reply::Busy
            } else {
                FileExt::unlock(&state.gc)?;
                let result = command(&state.configuration.nix_store)
                    .env("NIX_REMOTE", "daemon")
                    .arg("--gc")
                    .stdout(std::process::Stdio::null())
                    .status()
                    .await;
                FileExt::lock_shared(&state.gc)?;
                match result {
                    Ok(status) if status.success() => Reply::Applied,
                    Ok(_) => Reply::Failed,
                    Err(error) => return Err(error.into()),
                }
            }
        }
    };
    send(stream, &reply).await
}

async fn admit(stream: &mut UnixStream, state: &Mutex<State>) -> Result<(), Error> {
    let peer = stream.peer_cred()?;
    let request: Admission = receive(stream).await?;
    let state = state.lock().await;
    FileExt::try_lock_shared(&state.gc)?;
    let permitted = matches!(state.admission, AdmissionState::Serving)
        && request.version == 1
        && peer.uid() == request.role.uid()
        && state
            .leases
            .get(&request.id)
            .is_some_and(|record| !record.closing);
    send(
        stream,
        &if permitted {
            Reply::Applied
        } else {
            Reply::Rejected
        },
    )
    .await
}

pub async fn serve() -> Result<(), Error> {
    role_root()?;
    let configuration = configuration()?;
    let _: bootstrap::Configuration =
        serde_json::from_slice(&private_file(&Path::new(DATA).join("bootstrap.json"))?)?;
    let leases = records()?;
    // Duplicate the supervisor's open-file description; reopening its path would
    // lose protection when this controller dies before the supervisor stops Nix.
    let gc = File::from(rustix::io::dup(io::stdin()).map_err(io::Error::from)?);
    let actual = gc.metadata()?;
    let expected = fs::metadata("/nix/var/nix/gc.lock")?;
    if !actual.is_file() || actual.dev() != expected.dev() || actual.ino() != expected.ino() {
        return Err(Error::Invalid);
    }
    FileExt::lock_shared(&gc)?;
    let state = Arc::new(Mutex::new(State {
        admission: AdmissionState::Serving,
        leases,
        gc,
        configuration,
    }));
    let control_path = Path::new(RUN).join("control.sock");
    let admission_path = Path::new(RUN).join("admission.sock");
    let control = UnixListener::bind(&control_path)?;
    fs::set_permissions(control_path, fs::Permissions::from_mode(0o600))?;
    let admission = UnixListener::bind(&admission_path)?;
    fs::set_permissions(admission_path, fs::Permissions::from_mode(0o666))?;
    loop {
        let (mut stream, admin) = tokio::select! {
            accepted = control.accept() => (accepted?.0, true),
            accepted = admission.accept() => (accepted?.0, false),
        };
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let result = if admin {
                administer(&mut stream, &state).await
            } else {
                admit(&mut stream, &state).await
            };
            if let Err(error) = result {
                tracing::warn!(%error, "native pool request refused");
            }
        });
    }
}

/// Validate fixed startup storage without formatting, resizing or repairing retained data.
pub fn storage() -> Result<bootstrap::Storage, Error> {
    role_root()?;
    let configuration: bootstrap::Configuration =
        serde_json::from_slice(&private_file(&Path::new(DATA).join("bootstrap.json"))?)?;
    if configuration.storage() != bootstrap::Storage::Allocate {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(Path::new(DATA).join("store.ext4"))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
            || metadata.len() != bootstrap::CAPACITY
            || FileExt::allocated_size(&file)? < bootstrap::CAPACITY
        {
            return Err(Error::Invalid);
        }
    }
    Ok(configuration.storage())
}

fn startup_software() -> Result<image::StorePath, Error> {
    let entrypoint = fs::canonicalize("/bin/wrix-nix-pool")?;
    let software = entrypoint
        .parent()
        .and_then(Path::parent)
        .ok_or(Error::Invalid)?;
    let software = image::StorePath::try_from(software.to_str().ok_or(Error::Invalid)?.to_owned())?;
    if entrypoint != Path::new(software.as_str()).join("bin/wrix-nix-pool") {
        return Err(Error::Invalid);
    }
    Ok(software)
}

/// Seal the initial immutable software and administration identity without replacing old records.
pub fn seal() -> Result<(), Error> {
    role_root()?;
    let configuration: bootstrap::Configuration =
        serde_json::from_slice(&private_file(&Path::new(DATA).join("bootstrap.json"))?)?;
    if configuration.storage() == bootstrap::Storage::Resume {
        return Err(Error::Invalid);
    }
    restart::seal(
        Path::new(DATA),
        &startup_software()?,
        &configuration.admin_key,
    )
}

/// Consume a clean guest checkpoint before opening retained native state; never proves host release.
pub fn resume() -> Result<(), Error> {
    if storage()? != bootstrap::Storage::Resume {
        return Err(Error::Invalid);
    }
    let configuration: bootstrap::Configuration =
        serde_json::from_slice(&private_file(&Path::new(DATA).join("bootstrap.json"))?)?;
    restart::begin(
        Path::new(DATA),
        &startup_software()?,
        &configuration.admin_key,
    )
}

pub async fn control() -> Result<(), Error> {
    role_root()?;
    let mut bytes = Vec::new();
    tokio::io::stdin()
        .take(MAX_MESSAGE + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > MAX_MESSAGE {
        return Err(Error::Invalid);
    }
    let request: Operation = serde_json::from_slice(&bytes)?;
    let mut stream = UnixStream::connect(Path::new(RUN).join("control.sock")).await?;
    send(&mut stream, &request).await?;
    let response: Reply = if matches!(request, Operation::Collect {}) {
        receive_frame(&mut stream).await?
    } else {
        receive(&mut stream).await?
    };
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&bytes).await?;
    stdout.flush().await?;
    Ok(())
}

pub fn keys(user: &str) -> Result<Vec<String>, Error> {
    role_root()?;
    let configuration = configuration()?;
    let text = configuration.helper.to_str().ok_or(Error::Invalid)?;
    if !text.starts_with("/nix/store/")
        || !text.ends_with("/bin/wrix-nix-pool-owner")
        || text
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'\"' | b'\'' | b' '))
    {
        return Err(Error::Invalid);
    }
    if user == "root" {
        let bootstrap: bootstrap::Configuration =
            serde_json::from_slice(&private_file(&Path::new(DATA).join("bootstrap.json"))?)?;
        return Ok(vec![format!(
            "restrict,command=\"{text} control\" {}",
            bootstrap.admin_key.as_str()
        )]);
    }
    let role = Role::parse(user)?;
    Ok(records()?
        .values()
        .filter(|record| !record.closing)
        .map(|record| {
            format!(
                "restrict,command=\"{text} {} {}\" {}",
                role.name(),
                record.id.as_str(),
                record.key.as_str()
            )
        })
        .collect())
}

pub async fn session(role: Role, id: lease::Id) -> Result<std::process::ExitCode, Error> {
    if rustix::process::geteuid().as_raw() != role.uid() {
        return Err(Error::Unauthorized);
    }
    tracing::info!(
        role = role.name(),
        lease = id.as_str(),
        "native pool session starting"
    );
    let configuration = configuration()?;
    let mut stream = UnixStream::connect(Path::new(RUN).join("admission.sock")).await?;
    send(
        &mut stream,
        &Admission {
            version: 1,
            id,
            role,
        },
    )
    .await?;
    if !matches!(receive::<Reply>(&mut stream).await?, Reply::Applied) {
        return Err(Error::Unauthorized);
    }
    let status = match role {
        Role::Client => {
            command(&configuration.nix_daemon)
                .args(["--stdio", "--force-untrusted"])
                .status()
                .await?
        }
        Role::Export => {
            command(&configuration.sftp_server)
                .args(["-e", "-R", "-d", "/nix/store"])
                .status()
                .await?
        }
    };
    match status.code() {
        Some(code) => Ok(std::process::ExitCode::from(
            u8::try_from(code).map_err(|_| Error::Invalid)?,
        )),
        None => Ok(std::process::ExitCode::FAILURE),
    }
}
