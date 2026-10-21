//! Bounded read-only membership attestation; native namespace assignment is separate.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
    },
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

use crate::host::Host;

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// {0}
    Host(#[from] crate::host::Error),
    /// resolver I/O failed: {0}
    Io(#[from] std::io::Error),
    /// invalid workspace input: {0}
    Invalid(String),
    /// workspace observation changed; retry or explicitly readmit the checkout
    Conflict,
    /// unsupported resolver state
    Unsupported,
    /// unsupported Git administrative routing file
    PrivateState,
}

const LIMIT: usize = 16 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Serialize)]
struct Request {
    version: u8,
    caller_directory_b64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    version: u8,
    membership: String,
    checkout_root_b64: String,
    primary_root_b64: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Membership(String);

impl Membership {
    pub fn parse(value: &str) -> Result<Self, Error> {
        if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
            return Err(Error::Invalid("invalid external membership".into()));
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct Checkout {
    root: PathBuf,
    git: PathBuf,
    common: PathBuf,
    evidence: Vec<u8>,
    routing: Vec<(PathBuf, Option<Vec<u8>>)>,
}

#[derive(Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Evidence {
    root: (u64, u64),
    git: (u64, u64),
    common: (u64, u64),
    marker: (u64, u64),
}

impl Checkout {
    pub fn discover(path: &Path) -> Result<Self, Error> {
        Self::discover_before(path, Instant::now() + DEADLINE)
    }

    fn discover_before(path: &Path, deadline: Instant) -> Result<Self, Error> {
        let path = path.canonicalize()?;
        if !path.is_dir() {
            return Err(Error::Invalid("checkout must be a directory".into()));
        }
        let root = git_path(&path, "--show-toplevel", deadline)?;
        let git = git_path(&path, "--absolute-git-dir", deadline)?;
        let common = git_path(&path, "--git-common-dir", deadline)?;
        if !path.starts_with(&root) {
            return Err(Error::Invalid("caller is outside its Git checkout".into()));
        }
        let mut routes = vec![git.join("commondir")];
        if fs::symlink_metadata(root.join(".git"))?.is_file() {
            routes.push(root.join(".git"));
        }
        let routing = routes
            .into_iter()
            .map(|path| read_routing(&path).map(|bytes| (path, bytes)))
            .collect::<Result<Vec<_>, _>>()?;
        let evidence = serde_json::to_vec(&Evidence {
            root: observation(&root)?,
            git: observation(&git)?,
            common: observation(&common)?,
            marker: observation(&root.join(".git"))?,
        })
        .map_err(|error| Error::Invalid(error.to_string()))?;
        Ok(Self {
            root,
            git,
            common,
            evidence,
            routing,
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn git(&self) -> &Path {
        &self.git
    }
    pub fn common(&self) -> &Path {
        &self.common
    }
    pub fn evidence(&self) -> &[u8] {
        &self.evidence
    }
    pub(crate) fn matches(&self, other: &Self) -> bool {
        self.root.as_os_str() == other.root.as_os_str()
            && self.git.as_os_str() == other.git.as_os_str()
            && self.common.as_os_str() == other.common.as_os_str()
            && self.evidence == other.evidence
    }

    pub fn recheck(&self) -> Result<(), Error> {
        let current = serde_json::to_vec(&Evidence {
            root: observation(&self.root)?,
            git: observation(&self.git)?,
            common: observation(&self.common)?,
            marker: observation(&self.root.join(".git"))?,
        })
        .map_err(|error| Error::Invalid(error.to_string()))?;
        if current != self.evidence {
            return Err(Error::Conflict);
        }
        for (path, bytes) in &self.routing {
            if &read_routing(path)? != bytes {
                return Err(Error::Conflict);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Resolution {
    caller: PathBuf,
    membership: Membership,
    checkout: Checkout,
    primary: Checkout,
}

impl Resolution {
    pub(crate) fn matches(&self, other: &Self) -> bool {
        self.caller.as_os_str() == other.caller.as_os_str()
            && self.membership == other.membership
            && self.checkout.matches(&other.checkout)
            && self.primary.matches(&other.primary)
    }
    pub fn caller(&self) -> &Path {
        &self.caller
    }
    pub const fn membership(&self) -> &Membership {
        &self.membership
    }
    pub const fn checkout(&self) -> &Checkout {
        &self.checkout
    }
    pub const fn primary(&self) -> &Checkout {
        &self.primary
    }
    pub fn recheck(&self) -> Result<(), Error> {
        self.checkout.recheck()?;
        self.primary.recheck()?;
        if !self
            .caller
            .canonicalize()?
            .starts_with(self.checkout.root())
        {
            return Err(Error::Conflict);
        }
        Ok(())
    }
}

pub fn resolve(host: &Host, executable: &Path, caller: &Path) -> Result<Resolution, Error> {
    host.check()?;
    let deadline = Instant::now() + DEADLINE;
    let caller = caller.canonicalize()?;
    if !caller.is_dir() {
        return Err(Error::Invalid("caller must be a directory".into()));
    }
    let metadata = fs::symlink_metadata(executable)?;
    if !executable.is_absolute()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o022 != 0
        || ![0, rustix::process::geteuid().as_raw()].contains(&metadata.uid())
    {
        return Err(Error::PrivateState);
    }
    let request = serde_json::to_vec(&Request {
        version: 1,
        caller_directory_b64: encode(&caller),
    })
    .map_err(|error| Error::Invalid(error.to_string()))?;
    if request.len() > LIMIT {
        return Err(Error::Invalid("resolver request exceeds 16 KiB".into()));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let bytes = runtime.block_on(execute(executable, "resolve", &request, deadline))?;
    let reply: Reply = serde_json::from_slice(&bytes)
        .map_err(|error| Error::Invalid(format!("invalid resolver response: {error}")))?;
    if reply.version != 1 {
        return Err(Error::Unsupported);
    }
    let membership = Membership::parse(&reply.membership)?;
    let root = decode(&reply.checkout_root_b64)?;
    let primary_root = decode(&reply.primary_root_b64)?;
    let checkout = Checkout::discover_before(&root, deadline)?;
    let primary = Checkout::discover_before(&primary_root, deadline)?;
    if checkout.root() != root
        || primary.root() != primary_root
        || !caller.starts_with(&root)
        || Checkout::discover_before(&caller, deadline)?.root() != root
    {
        return Err(Error::Invalid(
            "resolver checkout roots do not match the actual caller".into(),
        ));
    }
    let resolution = Resolution {
        caller,
        membership,
        checkout,
        primary,
    };
    resolution.recheck()?;
    Ok(resolution)
}

pub fn encode(path: &Path) -> String {
    STANDARD.encode(path.as_os_str().as_bytes())
}

fn decode(value: &str) -> Result<PathBuf, Error> {
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| Error::Invalid("invalid path encoding".into()))?;
    if bytes.is_empty()
        || bytes.len() > 4096
        || bytes.contains(&0)
        || STANDARD.encode(&bytes) != value
    {
        return Err(Error::Invalid("invalid path encoding or length".into()));
    }
    let path = PathBuf::from(OsString::from_vec(bytes));
    if !path.is_absolute() || path.canonicalize()?.as_os_str() != path.as_os_str() {
        return Err(Error::Invalid(
            "resolver roots must be absolute canonical paths".into(),
        ));
    }
    Ok(path)
}

pub(crate) fn check_evidence(bytes: &[u8]) -> Result<(), Error> {
    let _: Evidence =
        serde_json::from_slice(bytes).map_err(|error| Error::Invalid(error.to_string()))?;
    Ok(())
}

fn read_routing(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            i32::try_from((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits())
                .map_err(|_| Error::Unsupported)?,
        )
        .open(path);
    let file = match file {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        return Err(Error::PrivateState);
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::Invalid("Git routing file exceeds 1 MiB".into()));
    }
    Ok(Some(bytes))
}

fn observation(path: &Path) -> Result<(u64, u64), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(Error::PrivateState);
    }
    Ok((metadata.dev(), metadata.ino()))
}

fn git_path(caller: &Path, query: &str, deadline: Instant) -> Result<PathBuf, Error> {
    let git = find_git()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let output = runtime.block_on(async {
        let mut command = controlled(&git);
        command
            .args(["rev-parse", "--path-format=absolute", query])
            .current_dir(caller)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0");
        run(command, &[], deadline).await
    })?;
    let bytes = output
        .strip_suffix(b"\n")
        .ok_or_else(|| Error::Invalid("Git returned an incomplete path".into()))?;
    let path = PathBuf::from(OsString::from_vec(bytes.to_vec()));
    if !path.is_absolute() {
        return Err(Error::Invalid("Git returned a relative path".into()));
    }
    Ok(path.canonicalize()?)
}

fn find_git() -> Result<PathBuf, Error> {
    let search =
        std::env::var_os("PATH").ok_or_else(|| Error::Invalid("Git is unavailable".into()))?;
    for directory in std::env::split_paths(&search).filter(|path| path.is_absolute()) {
        let path = directory.join("git");
        match fs::metadata(&path) {
            Ok(metadata)
                if metadata.is_file()
                    && metadata.mode() & 0o111 != 0
                    && metadata.mode() & 0o022 == 0 =>
            {
                return Ok(path.canonicalize()?);
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::Invalid(
        "Git is unavailable on the trusted host PATH".into(),
    ))
}

fn controlled(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .env_clear()
        .current_dir("/")
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("HOME", "/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    command
}

async fn execute(
    executable: &Path,
    argument: &str,
    input: &[u8],
    deadline: Instant,
) -> Result<Vec<u8>, Error> {
    let mut command = controlled(executable);
    command.arg(argument);
    run(command, input, deadline).await
}

async fn bounded(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    reader
        .take(u64::try_from(LIMIT + 1).map_err(|_| Error::Unsupported)?)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > LIMIT {
        return Err(Error::Invalid("resolver output exceeds 16 KiB".into()));
    }
    Ok(bytes)
}

async fn run(mut command: Command, input: &[u8], deadline: Instant) -> Result<Vec<u8>, Error> {
    use tokio::signal::unix::{SignalKind, signal};
    if Instant::now() >= deadline {
        return Err(Error::Invalid(
            "resolver execution deadline exceeded".into(),
        ));
    }
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut child = command.spawn()?;
    let child_id = child.id().ok_or(Error::Unsupported)?;
    let raw_id = i32::try_from(child_id).map_err(|_| Error::Unsupported)?;
    let pid = rustix::process::Pid::from_raw(raw_id).ok_or(Error::Unsupported)?;
    let mut stdin = child.stdin.take().ok_or(Error::Unsupported)?;
    let stdout = child.stdout.take().ok_or(Error::Unsupported)?;
    let stderr = child.stderr.take().ok_or(Error::Unsupported)?;
    let outcome = {
        let work = async {
            let write = async move {
                stdin.write_all(input).await?;
                stdin.shutdown().await?;
                drop(stdin);
                Ok::<(), Error>(())
            };
            let ((), stdout, _stderr) = tokio::try_join!(write, bounded(stdout), bounded(stderr))?;
            loop {
                let status = rustix::process::waitid(
                    rustix::process::WaitId::Pid(pid),
                    rustix::process::WaitIdOptions::EXITED
                        | rustix::process::WaitIdOptions::NOHANG
                        | rustix::process::WaitIdOptions::NOWAIT,
                )
                .map_err(std::io::Error::from)?;
                if let Some(status) = status {
                    if status.exit_status() != Some(0) {
                        return Err(Error::Invalid("resolver process failed".into()));
                    }
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(stdout)
        };
        tokio::select! {
            result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), work) => result.map_err(|_| Error::Invalid("resolver execution deadline exceeded".into())).and_then(std::convert::identity),
            _ = interrupt.recv() => Err(Error::Invalid("resolver interrupted".into())),
            _ = terminate.recv() => Err(Error::Invalid("resolver terminated".into())),
            _ = hangup.recv() => Err(Error::Invalid("resolver disconnected".into())),
        }
    };
    match rustix::process::kill_process_group(pid, rustix::process::Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => (),
        Err(error) => return Err(Error::Io(error.into())),
    }
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .map_err(|_| Error::Invalid("resolver child cleanup deadline exceeded".into()))??;
    outcome
}
