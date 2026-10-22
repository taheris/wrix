//! External native cache-config fixture; it never models Wrix admission logic.

use std::{
    fs,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use serde_json::{Value, json};

use super::TestResult;

pub const KEY: &str = "wrix-cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

pub struct Server {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl Server {
    /// Binds only after the external runtime has recorded its native HTTP port.
    pub fn watch(root: &Path, running_port: Option<u16>) -> Self {
        Self::start(root, running_port, None)
    }

    pub fn reserved(root: &Path, listener: TcpListener) -> io::Result<Self> {
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        Ok(Self::start(root, Some(port), Some(listener)))
    }

    fn start(root: &Path, running_port: Option<u16>, listener: Option<TcpListener>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&stop);
        let root = root.to_owned();
        let worker = thread::spawn(move || run(&root, running_port, listener, &signal));
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(Err(error)) if thread::panicking() => {
                    tracing::warn!(%error, "Celler fixture cleanup failed during an earlier failure");
                }
                Ok(result) => assert!(result.is_ok(), "Celler fixture failed: {result:?}"),
                Err(_) if thread::panicking() => {
                    tracing::warn!("Celler fixture worker panicked during an earlier failure");
                }
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    }
}

fn run(
    root: &Path,
    mut port: Option<u16>,
    mut listener: Option<TcpListener>,
    stop: &AtomicBool,
) -> io::Result<()> {
    while !stop.load(Ordering::Relaxed) {
        if let Some(recorded) = recorded_port(&root.join("run.argv"))?
            && port != Some(recorded)
        {
            listener = None;
            port = Some(recorded);
        }
        if listener.is_none()
            && let Some(port) = port
        {
            let bound = TcpListener::bind(("127.0.0.1", port))?;
            bound.set_nonblocking(true)?;
            listener = Some(bound);
        }
        if let Some(listener) = &listener {
            match listener.accept() {
                Ok((stream, _)) => serve(root, stream)?,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                Err(error) => return Err(error),
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn recorded_port(path: &Path) -> io::Result<Option<u16>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.split(['\0', '\n']).find_map(|arg| {
        let port = arg.strip_prefix("127.0.0.1:")?.strip_suffix(":8080")?;
        port.parse().ok()
    }))
}

fn serve(root: &Path, mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut byte = [0];
        if stream.read(&mut byte)? == 0 {
            return Ok(());
        }
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
        if bytes.len() > 8192 {
            return Err(io::Error::other("oversized fixture request"));
        }
    };
    let header = String::from_utf8_lossy(&bytes);
    let length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>())
        })
        .transpose()
        .map_err(io::Error::other)?
        .unwrap_or(0);
    let request = header
        .lines()
        .next()
        .ok_or_else(|| io::Error::other("missing request"))?
        .to_owned();
    let authenticated = header.lines().any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        name.eq_ignore_ascii_case("authorization") && value.trim().starts_with("Bearer ")
    });
    bytes.resize(boundary + length, 0);
    stream.read_exact(&mut bytes[boundary..])?;
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("");
    let route = parts.next().unwrap_or("");
    let forced = match fs::read_to_string(root.join("celler-status")) {
        Ok(status) => Some(status),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let valid = route.starts_with("/_api/v1/cache-config/wrix-") && authenticated;
    let status = forced
        .as_deref()
        .unwrap_or(if valid { "200" } else { "403" });
    let policy = root.join(if route.ends_with("/wrix-common-v1") {
        "celler-common-policy.json"
    } else {
        "celler-policy.json"
    });
    let body = match method {
        "GET" => {
            let period =
                match fs::read(&policy) {
                    Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
                        .map_err(io::Error::other)?["retention_period"]
                        .clone(),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        json!({"Period":2_592_000})
                    }
                    Err(error) => return Err(error),
                };
            json!({"public_key": KEY,"is_public":false,"retention_period":period})
        }
        "PATCH" => {
            let value: Value =
                serde_json::from_slice(&bytes[boundary..]).map_err(io::Error::other)?;
            fs::write(&policy, serde_json::to_vec(&value)?)?;
            Value::Null
        }
        _ => Value::Null,
    };
    let response = serde_json::to_vec(&body)?;
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("celler-requests"))?
        .write_all(format!("{method} {route}\n").as_bytes())?;
    write!(
        stream,
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n",
        response.len()
    )?;
    stream.write_all(&response)
}

#[test]
fn fixture_native_configuration_matches_production_registration_protocol() -> TestResult {
    let root = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let _server = Server::reserved(root.path(), listener)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = wrix_cache::backend::Client::new(
        &format!("http://127.0.0.1:{port}/"),
        wrix_cache::backend::publication::registration::private_namespace(
            wrix_core::path::Workspace::from_path(root.path())?.hash(),
        )?,
        "fixture-authority",
    )?;
    runtime.block_on(async {
        assert_eq!(
            client.register(Duration::from_hours(720)).await?.as_str(),
            KEY
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}
