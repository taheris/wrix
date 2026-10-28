//! Fixture-owned host transports run the actual gateway and patched native compiler.
use super::TestResult;
use std::{
    fs, io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct Server {
    pub socket: PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<thread::JoinHandle<io::Result<()>>>,
}

#[derive(serde::Deserialize)]
#[serde(tag = "role", rename_all = "kebab-case")]
enum Request {
    Gateway {
        data: std::net::SocketAddr,
        management: std::net::SocketAddr,
        key: wrix_service::gateway::Secret,
    },
    CompilerReady,
    Forward {
        listen: std::net::SocketAddr,
        target: std::net::SocketAddr,
    },
}

struct Compiler {
    child: Child,
    root: PathBuf,
    binary: PathBuf,
}
impl Compiler {
    fn start(root: &Path, cache: &Path) -> io::Result<Self> {
        let binary = std::env::var_os("WRIX_TEST_SCCACHE")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("missing patched native compiler fixture"))?;
        let root = root.join("native-compiler");
        wrix_core::private_directory::prepare_with_parents(&root.join("execution/probe/workspace"))
            .map_err(io::Error::other)?;
        wrix_core::private_directory::prepare_with_parents(cache).map_err(io::Error::other)?;
        fs::File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(cache.join("owner.lock"))?;
        let child = Command::new("flock")
            .args(["--exclusive", "--nonblock", "--no-fork"])
            .arg(cache.join("owner.lock"))
            .arg(&binary)
            .env("SCCACHE_SERVER_UDS", root.join("sccache.sock"))
            .env("SCCACHE_START_SERVER", "1")
            .env("SCCACHE_NO_DAEMON", "1")
            .env("SCCACHE_DIR", cache)
            .env("SCCACHE_IDLE_TIMEOUT", "0")
            .env("SCCACHE_CACHE_SIZE", "50G")
            .env("WRIX_SCCACHE_MANAGED", "1")
            .env("WRIX_COMPILER_VIEW_ROOT", root.join("execution"))
            .env("WRIX_SCCACHE_DRAIN_SOCKET", root.join("drain.sock"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(root.join("stderr"))?)
            .spawn()?;
        fs::write(root.join("pid"), child.id().to_string())?;
        let mut compiler = Self {
            child,
            root,
            binary,
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !compiler.root.join("sccache.sock").exists() {
            if compiler.child.try_wait()?.is_some() || std::time::Instant::now() >= deadline {
                return Err(io::Error::other("native compiler did not start"));
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        Ok(compiler)
    }
    fn ready(&self) -> io::Result<()> {
        wrix_service::lifecycle::compiler::ready_on(
            &self.root.join("sccache.sock"),
            &self.root.join("pid"),
            &self.root.join("execution"),
            &self.binary,
        )
        .map_err(io::Error::other)
    }
}
impl Drop for Compiler {
    fn drop(&mut self) {
        if let Err(error) = (|| -> io::Result<()> {
            if self.child.try_wait()?.is_none() {
                self.child.kill()?;
            }
            self.child.wait()?;
            Ok(())
        })() {
            tracing::error!(%error, "fixture compiler cleanup failed");
        }
    }
}
impl Server {
    pub fn start(root: &Path, cache: &Path) -> TestResult<Self> {
        let socket = root.join("roles.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let root = root.to_owned();
        let cache = cache.to_owned();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let task = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move {
                let listener = tokio::net::UnixListener::from_std(listener)?;
                let mut gateways = std::collections::BTreeMap::new();
                let mut forwards = std::collections::BTreeMap::new();
                let mut compiler: Option<Compiler> = None;
                loop {
                    tokio::select! {
                        _ = &mut stopped => break,
                        accepted = listener.accept() => {
                            let (stream, _) = accepted?;
                            let mut stream = BufReader::new(stream);
                            let mut frame = String::new();
                            stream.read_line(&mut frame).await?;
                            let result = match serde_json::from_str::<Request>(&frame).map_err(io::Error::other) {
                                Ok(Request::Gateway {data, management, key}) => {
                                    start_gateway(&mut gateways, data, management, key).await
                                }
                                Ok(Request::CompilerReady) => {
                                    compiler.take().map_or_else(|| Compiler::start(&root, &cache), Ok).and_then(|started| {
                                        let ready = started.ready();
                                        compiler = Some(started);
                                        ready
                                    })
                                }
                                Ok(Request::Forward { listen, target }) => start_forward(&mut forwards, listen, target).await,
                                Err(error) => Err(error),
                            };
                            let reply = match result { Ok(()) => serde_json::json!({"ok":true}), Err(error) => serde_json::json!({"error":error.to_string()}) };
                            stream.get_mut().write_all(format!("{reply}\n").as_bytes()).await?;
                        }
                    }
                }
                for (_, (stop, task)) in gateways {
                    let _ = stop.send(());
                    task.await.map_err(io::Error::other)?.map_err(io::Error::other)?;
                }
                for (_, task) in forwards {
                    task.abort();
                    if let Err(error) = task.await && !error.is_cancelled() {
                        return Err(io::Error::other(error));
                    }
                }
                Ok(())
            })
        });
        Ok(Self {
            socket,
            stop: Some(stop),
            task: Some(task),
        })
    }
}

async fn start_forward(
    forwards: &mut std::collections::BTreeMap<
        std::net::SocketAddr,
        tokio::task::JoinHandle<io::Result<()>>,
    >,
    listen: std::net::SocketAddr,
    target: std::net::SocketAddr,
) -> io::Result<()> {
    if let Some(task) = forwards.remove(&listen) {
        task.abort();
        if let Err(error) = task.await
            && !error.is_cancelled()
        {
            return Err(io::Error::other(error));
        }
    }
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut incoming, _) = accepted?;
                    connections.spawn(async move {
                        let mut outgoing = tokio::net::TcpStream::connect(target).await?;
                        tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await?;
                        io::Result::Ok(())
                    });
                }
                Some(result) = connections.join_next() => {
                    if let Err(error) = result { return Err(io::Error::other(error)); }
                }
            }
        }
    });
    forwards.insert(listen, task);
    Ok(())
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take()
            && !matches!(task.join(), Ok(Ok(())))
        {
            tracing::error!("native role fixture did not stop cleanly");
        }
    }
}

type Gateways = std::collections::BTreeMap<
    std::net::SocketAddr,
    (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), wrix_service::gateway::Error>>,
    ),
>;

async fn start_gateway(
    gateways: &mut Gateways,
    data: std::net::SocketAddr,
    management: std::net::SocketAddr,
    key: wrix_service::gateway::Secret,
) -> io::Result<()> {
    // A fresh external VM launch replaces its old fixture-owned process. Wait for
    // the real gateway to close transports before rebinding the published ports.
    if let Some((stop, task)) = gateways.remove(&management) {
        let _ = stop.send(());
        task.await
            .map_err(io::Error::other)?
            .map_err(io::Error::other)?;
    }
    let data = tokio::net::TcpListener::bind(data).await?;
    let management_listener = tokio::net::TcpListener::bind(management).await?;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(wrix_service::gateway::run(
        data,
        management_listener,
        key,
        vec![management],
        std::num::NonZeroUsize::MIN.saturating_add(1023),
        async {
            let _ = stopped.await;
        },
    ));
    gateways.insert(management, (stop, task));
    Ok(())
}
