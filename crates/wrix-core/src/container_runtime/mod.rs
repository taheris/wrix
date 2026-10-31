//! Host-owned runtime selection. Never substitute a Wrix-bundled container engine.

use std::{
    env,
    ffi::OsStr,
    fs,
    io::{self, Read},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{OnceLock, mpsc},
    time::{Duration, Instant},
};

use serde::Deserialize;
use wait_timeout::ChildExt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Engine {
    Podman,
    Container,
}

static PODMAN: OnceLock<Result<PathBuf, String>> = OnceLock::new();
static CONTAINER: OnceLock<Result<PathBuf, String>> = OnceLock::new();
static CHECKED: OnceLock<Result<(), String>> = OnceLock::new();

impl Engine {
    pub const fn native() -> Self {
        if cfg!(target_os = "macos") {
            Self::Container
        } else {
            Self::Podman
        }
    }

    pub fn for_program(program: &OsStr) -> Self {
        if Path::new(program).file_name() == Some(OsStr::new("container")) {
            Self::Container
        } else {
            Self::Podman
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Container => "container",
        }
    }

    pub fn current() -> Self {
        env::var_os("WRIX_CONTAINER_RUNTIME")
            .map_or_else(Self::native, |value| Self::for_program(&value))
    }

    /// Freeze one canonical executable per engine for this process, including cleanup.
    pub fn path(self) -> io::Result<PathBuf> {
        let cache = match self {
            Self::Podman => &PODMAN,
            Self::Container => &CONTAINER,
        };
        cached(cache.get_or_init(|| {
            let explicit = env::var_os("WRIX_CONTAINER_RUNTIME");
            let explicit = explicit
                .as_deref()
                .filter(|value| Self::for_program(value) == self);
            let path = env::var_os("WRIX_HOST_RUNTIME_PATH").or_else(|| env::var_os("PATH"));
            let system = if self == Self::Podman {
                vec![
                    PathBuf::from("/run/current-system/sw/bin/podman"),
                    PathBuf::from("/usr/bin/podman"),
                ]
            } else {
                Vec::new()
            };
            resolve(self, explicit, path.as_deref(), &system).map_err(|error| error.to_string())
        }))
        .cloned()
    }

    pub fn command(self) -> io::Result<Command> {
        let path = self.path()?;
        let mut command = Command::new(&path);
        command.env("WRIX_CONTAINER_RUNTIME", &path);
        Ok(command)
    }
}

fn cached<T>(value: &Result<T, String>) -> io::Result<&T> {
    value
        .as_ref()
        .map_err(|error| io::Error::other(error.clone()))
}

fn executable(path: &Path) -> io::Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file() && metadata.permissions().mode() & 0o111 != 0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn resolve(
    engine: Engine,
    explicit: Option<&OsStr>,
    search: Option<&OsStr>,
    system: &[PathBuf],
) -> io::Result<PathBuf> {
    let candidates = if let Some(explicit) = explicit {
        let path = Path::new(explicit);
        if explicit.is_empty() {
            return Err(io::Error::other("WRIX_CONTAINER_RUNTIME must not be empty"));
        }
        if path.is_absolute() || path.components().count() > 1 {
            vec![path.to_path_buf()]
        } else {
            env::split_paths(search.unwrap_or_default())
                .map(|root| root.join(path))
                .collect()
        }
    } else {
        system
            .iter()
            .cloned()
            .chain(
                env::split_paths(search.unwrap_or_default()).map(|root| root.join(engine.name())),
            )
            .collect()
    };
    for path in candidates {
        if executable(&path)? {
            return fs::canonicalize(path);
        }
    }
    Err(io::Error::other(format!(
        "host {} runtime {} is unavailable; install/configure the host runtime or set WRIX_CONTAINER_RUNTIME to its executable; Wrix will not substitute a bundled engine",
        engine.name(),
        explicit
            .unwrap_or_else(|| OsStr::new(engine.name()))
            .to_string_lossy()
    )))
}

pub fn command(program: &str) -> io::Result<Command> {
    match program {
        "podman" => Engine::Podman.command(),
        "container" => Engine::Container.command(),
        _ => Ok(Command::new(program)),
    }
}

/// Carry the selected runtime into a Wrix child, before its package adjusts PATH.
pub fn inherit(command: &mut Command) -> io::Result<()> {
    command.env("WRIX_CONTAINER_RUNTIME", Engine::current().path()?);
    Ok(())
}

#[derive(Deserialize)]
struct Info {
    host: Host,
    store: Store,
    version: Version,
}
#[derive(Deserialize)]
struct Host {
    security: Security,
}
#[derive(Deserialize)]
struct Security {
    rootless: bool,
}
#[derive(Deserialize)]
struct Store {
    #[serde(rename = "graphRoot")]
    graph_root: PathBuf,
    #[serde(rename = "runRoot")]
    run_root: PathBuf,
}
#[derive(Deserialize)]
struct Version {
    #[serde(rename = "Version")]
    version: String,
}

fn validate_info(bytes: &[u8]) -> io::Result<()> {
    let info: Info = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    let major = info
        .version
        .version
        .split('.')
        .next()
        .and_then(|value| value.parse::<u16>().ok());
    if !matches!(major, Some(5 | 6)) {
        return Err(io::Error::other(
            "this Wrix client supports Podman major versions 5 and 6; update the pinned Wrix client to admit other runtime versions",
        ));
    }
    if !info.host.security.rootless
        || !info.store.graph_root.is_absolute()
        || !info.store.run_root.is_absolute()
        || info.store.graph_root.starts_with("/var/lib/containers")
        || info.store.run_root.starts_with("/run/containers")
    {
        return Err(io::Error::other(
            "Podman must provide rootless user storage, not system-owned container storage; reconcile host runtime/configuration explicitly; do not reset storage or change its ownership",
        ));
    }
    Ok(())
}

const BUDGET: Duration = Duration::from_secs(15);
const OUTPUT_LIMIT: u64 = 64 * 1024;

fn capture(mut command: Command, budget: Duration) -> io::Result<Output> {
    let deadline = Instant::now() + budget;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("runtime stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("runtime stderr unavailable"))?;
    let read = |pipe: Box<dyn Read + Send>| {
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe
                .take(OUTPUT_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            // The caller can time out before a descendant releases the pipe.
            let _ = tx.send(result);
        });
        rx
    };
    let stdout = read(Box::new(stdout));
    let stderr = read(Box::new(stderr));
    let Some(status) = child.wait_timeout(budget)? else {
        child.kill()?;
        child.wait()?;
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "runtime readiness exceeded 15 seconds",
        ));
    };
    let receive = |rx: mpsc::Receiver<io::Result<Vec<u8>>>| -> io::Result<Vec<u8>> {
        let bytes = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error))??;
        if bytes.len() as u64 > OUTPUT_LIMIT {
            return Err(io::Error::other("runtime readiness output exceeded 64 KiB"));
        }
        Ok(bytes)
    };
    Ok(Output {
        status,
        stdout: receive(stdout)?,
        stderr: receive(stderr)?,
    })
}

/// Bounded local readiness, before image installation, grants or service changes.
/// Podman itself may initialize its ordinary runtime metadata during `info`.
pub fn preflight() -> io::Result<()> {
    if Engine::current() != Engine::Podman {
        return Ok(());
    }
    cached(CHECKED.get_or_init(|| check_podman().map_err(|error| error.to_string()))).copied()
}

fn check_podman() -> io::Result<()> {
    let engine = Engine::Podman;
    let path = engine.path()?;
    let mut command = engine.command()?;
    command.arg("--version");
    let version = capture(command, BUDGET)?;
    let version = if version.status.success() {
        String::from_utf8_lossy(&version.stdout)
            .trim()
            .escape_debug()
            .to_string()
    } else {
        "unknown version".into()
    };
    let check = || {
        let mut command = engine.command()?;
        command.args(["info", "--format", "json"]);
        let output = capture(command, BUDGET)?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "{}",
                String::from_utf8_lossy(&output.stderr)
                    .trim()
                    .escape_debug()
            )));
        }
        validate_info(&output.stdout)
    };
    check().map_err(|error: io::Error| io::Error::other(format!(
        "host runtime preflight failed for {} ({version}): {error}; no Wrix image, grant or service changes were performed; Wrix will not try another runtime", path.display()
    )))
}

#[cfg(test)]
mod test;
