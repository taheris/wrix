//! Managed native sccache work/output lifecycle; never a connection-count drain.
use serde::Deserialize;
use std::{
    io,
    os::unix::fs::PermissionsExt,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
    runtime::Runtime,
    sync::{oneshot, watch},
};
#[path = "work_drain_state.rs"]
pub mod state;
use state::{Guard, Tracker};

static TRACKER: OnceLock<Arc<Tracker>> = OnceLock::new();
static STOP: OnceLock<watch::Sender<bool>> = OnceLock::new();
pub fn tracker() -> &'static Arc<Tracker> {
    TRACKER.get_or_init(Tracker::new)
}
fn stop() -> &'static watch::Sender<bool> {
    STOP.get_or_init(|| watch::channel(false).0)
}
pub fn configured() -> bool {
    std::env::var_os("WRIX_SCCACHE_DRAIN_SOCKET").is_some()
}

/// Managed cwd must resolve inside the originating execution's actually mounted view.
pub fn origin(
    cwd: &std::ffi::OsStr,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> io::Result<()> {
    if !configured() {
        return Ok(());
    }
    let mut selected = env
        .iter()
        .filter(|(name, _)| name == "WRIX_COMPILER_EXECUTION");
    let id = selected
        .next()
        .and_then(|(_, value)| value.to_str())
        .ok_or_else(|| io::Error::other("native compiler execution identity is missing"))?;
    if selected.next().is_some()
        || id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(io::Error::other(
            "invalid native compiler execution identity",
        ));
    }
    let root = std::env::var_os("WRIX_COMPILER_VIEW_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/run/wrix/compiler/execution".into());
    let root = root.canonicalize()?;
    let expected = root.join(id).join("workspace");
    let view = expected.canonicalize()?;
    let cwd = std::path::Path::new(cwd).canonicalize()?;
    if view != expected || !cwd.starts_with(&view) || !cwd.is_dir() {
        return Err(io::Error::other(
            "native compiler cwd is outside its originating mounted execution view",
        ));
    }
    Ok(())
}
pub fn admitted() -> io::Result<Option<Guard>> {
    if !configured() {
        return Ok(None);
    }
    tracker()
        .request(None, "compiler")
        .map(Some)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "native compiler admission is closed",
            )
        })
}
pub async fn stopped() {
    let mut signal = stop().subscribe();
    while !*signal.borrow_and_update() {
        if signal.changed().await.is_err() {
            return;
        }
    }
}

pub struct Running {
    path: std::path::PathBuf,
    finished: oneshot::Sender<bool>,
    manager: tokio::task::JoinHandle<io::Result<()>>,
}
impl Running {
    pub fn start(runtime: &Runtime) -> io::Result<Option<Self>> {
        let Some(path) = std::env::var_os("WRIX_SCCACHE_DRAIN_SOCKET") else {
            return Ok(None);
        };
        let listener = {
            let _entered = runtime.enter();
            UnixListener::bind(&path)?
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let (finished, completion) = oneshot::channel();
        let manager = runtime.spawn(control(listener, completion));
        Ok(Some(Self {
            path: path.into(),
            finished,
            manager,
        }))
    }
    /// All native service/storage references must be dropped before acknowledgment.
    pub fn finish(self, runtime: &Runtime) -> io::Result<()> {
        if tracker().status().phase != state::Phase::Releasing {
            return Err(io::Error::other(
                "native owner stopped without authorized clean release",
            ));
        }
        sync_cache()?;
        tracker().released();
        self.finished
            .send(true)
            .map_err(|_| io::Error::other("native release observer lost"))?;
        runtime.block_on(self.manager).map_err(io::Error::other)??;
        std::fs::remove_file(self.path)
    }
}

fn sync_cache() -> io::Result<()> {
    let root = std::env::var_os("SCCACHE_DIR")
        .ok_or_else(|| io::Error::other("managed native cache directory is required"))?;
    let root = std::path::PathBuf::from(root);
    if !root.exists() {
        return Ok(());
    }
    let mut pending = vec![root];
    let mut directories = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            }
        }
        directories.push(directory);
    }
    for directory in directories.into_iter().rev() {
        std::fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}

/// Fixed private-role command, without compilation, autostart or cache acquisition.
pub fn entry() -> Option<i32> {
    use std::io::Write;
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--wrix-work-drain")) {
        return None;
    }
    let result = (|| {
        let command = args
            .next()
            .and_then(|value| value.into_string().ok())
            .ok_or_else(|| io::Error::other("native work-drain command required"))?;
        if args.next().is_some()
            || !matches!(
                command.as_str(),
                "begin" | "quiesce" | "status" | "abort" | "release"
            )
        {
            return Err(io::Error::other("invalid native work-drain command"));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let response = runtime.block_on(async {
            let path = std::env::var_os("WRIX_SCCACHE_DRAIN_SOCKET")
                .unwrap_or_else(|| "/run/wrix/compiler/drain.sock".into());
            let mut stream = tokio::net::UnixStream::connect(path).await?;
            let mut request = serde_json::to_vec(&serde_json::json!({"command": command}))
                .map_err(io::Error::other)?;
            request.push(b'\n');
            stream.write_all(&request).await?;
            let mut response = Vec::new();
            BufReader::new(stream)
                .take(4097)
                .read_until(b'\n', &mut response)
                .await?;
            if response.len() > 4096 {
                return Err(io::Error::other("oversized native work-drain response"));
            }
            let value: serde_json::Value =
                serde_json::from_slice(&response).map_err(io::Error::other)?;
            if value["accepted"] != true {
                return Err(io::Error::other(
                    "native work-drain command was not accepted",
                ));
            }
            Ok::<_, io::Error>(response)
        })?;
        io::stdout().write_all(&response)
    })();
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("native compiler work-drain failed: {error}");
            1
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    command: Command,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Command {
    Begin,
    Quiesce,
    Status,
    Abort,
    Release,
}

async fn control(listener: UnixListener, completion: oneshot::Receiver<bool>) -> io::Result<()> {
    let mut completion = Some(completion);
    loop {
        let (stream, _) = listener.accept().await?;
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        let read = tokio::time::timeout(
            Duration::from_secs(5),
            (&mut stream).take(4097).read_line(&mut line),
        )
        .await;
        let size = match read {
            Ok(Ok(size)) => size,
            Ok(Err(error)) => {
                log::warn!("Native lifecycle request read failed: {error}");
                continue;
            }
            Err(error) => {
                log::warn!("Native lifecycle request read timed out: {error}");
                continue;
            }
        };
        let request = if size > 4096 {
            None
        } else {
            serde_json::from_str::<Request>(&line).ok()
        };
        let mut releasing = false;
        let mut accepted = match request.map(|request| request.command) {
            Some(Command::Begin) => tracker().begin(),
            Some(Command::Quiesce) => tracker().quiescent(),
            Some(Command::Status) => true,
            Some(Command::Abort) => tracker().abort(),
            Some(Command::Release) => {
                releasing = tracker().release();
                releasing
            }
            None => false,
        };
        if releasing {
            stop().send_replace(true);
            accepted = completion
                .take()
                .ok_or_else(|| io::Error::other("duplicate release"))?
                .await
                .map_err(io::Error::other)?;
        }
        let mut response = serde_json::to_vec(
            &serde_json::json!({"accepted": accepted, "status": tracker().status()}),
        )
        .map_err(io::Error::other)?;
        response.push(b'\n');
        let sent = async {
            stream.get_mut().write_all(&response).await?;
            stream.get_mut().shutdown().await
        }
        .await;
        if releasing {
            sent?;
            return Ok(());
        }
        if let Err(error) = sent {
            log::warn!("Native lifecycle observer disconnected: {error}");
        }
    }
}
