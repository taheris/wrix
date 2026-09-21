use std::{
    fs::{self, File},
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
mod test;

const MAX_CONNECTIONS: usize = 32;
const MAX_REQUEST_BYTES: usize = 8192;
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(300);

struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(super) fn serve(listener: &TcpListener, root: &Path) -> io::Result<()> {
    let root = Arc::new(root.canonicalize()?);
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        if active
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < MAX_CONNECTIONS).then_some(count + 1)
            })
            .is_err()
        {
            continue;
        }
        let slot = Slot(Arc::clone(&active));
        let root = Arc::clone(&root);
        let peer = match stream.peer_addr() {
            Ok(peer) => peer,
            Err(error) => {
                tracing::warn!(%error, "cache client disconnected before dispatch");
                continue;
            }
        };
        thread::Builder::new()
            .name("cache-client".into())
            .spawn(move || {
                let _slot = slot;
                if let Err(error) = handle_cache_request(stream, &root) {
                    tracing::warn!(%peer, %error, "cache client disconnected or request failed");
                }
            })?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Method {
    Get,
    Head,
}

#[derive(Clone, Copy)]
struct Request<'a> {
    method: Method,
    target: &'a str,
}

#[derive(Clone, Copy)]
enum Rejection {
    Malformed,
    Method,
    TooLarge,
}

impl Rejection {
    const fn status(self) -> &'static str {
        match self {
            Self::Malformed => "400 Bad Request",
            Self::Method => "405 Method Not Allowed",
            Self::TooLarge => "431 Request Header Fields Too Large",
        }
    }
}

fn parse_request(bytes: &[u8]) -> Result<Option<Request<'_>>, Rejection> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut parsed = httparse::Request::new(&mut headers);
    match parsed.parse(bytes) {
        Ok(httparse::Status::Partial) => return Ok(None),
        Ok(httparse::Status::Complete(_)) => (),
        Err(httparse::Error::TooManyHeaders) => return Err(Rejection::TooLarge),
        Err(_) => return Err(Rejection::Malformed),
    }
    let method = match parsed.method {
        Some("GET") => Method::Get,
        Some("HEAD") => Method::Head,
        _ => return Err(Rejection::Method),
    };
    Ok(Some(Request {
        method,
        target: parsed.path.ok_or(Rejection::Malformed)?,
    }))
}

pub(super) fn handle_cache_request(mut stream: TcpStream, root: &Path) -> io::Result<()> {
    let deadline = Instant::now() + HEADER_TIMEOUT;
    let mut buffer = [0; MAX_REQUEST_BYTES];
    let mut used = 0;
    loop {
        match parse_request(&buffer[..used]) {
            Ok(Some(request)) => {
                return serve_cache_path(&mut Output::new(&mut stream), root, request);
            }
            Err(rejection) => return reject(&mut Output::new(&mut stream), rejection),
            Ok(None) => (),
        }
        if used == buffer.len() {
            return reject(&mut Output::new(&mut stream), Rejection::TooLarge);
        }
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        let read = stream.read(&mut buffer[used..])?;
        if read == 0 {
            return reject(&mut Output::new(&mut stream), Rejection::Malformed);
        }
        used += read;
    }
}

fn serve_cache_path(output: &mut impl Write, root: &Path, request: Request<'_>) -> io::Result<()> {
    let Some(relative) = parse_cache_target(request.target) else {
        return write_missing(output, request.method);
    };
    let Some(path) = resolve_cache_file(root, relative)? else {
        return write_missing(output, request.method);
    };
    match request.method {
        Method::Head => write_headers(output, "200 OK", fs::metadata(path)?.len()),
        Method::Get => {
            let file = File::open(path)?;
            let size = file.metadata()?.len();
            write_headers(output, "200 OK", size)?;
            let copied = io::copy(&mut file.take(size), output)?;
            if copied != size {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "cache file was truncated during transfer",
                ));
            }
            Ok(())
        }
    }
}

fn resolve_cache_file(root: &Path, relative: &str) -> io::Result<Option<PathBuf>> {
    let root = root.canonicalize()?;
    let path = match root.join(relative).canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok((path.starts_with(&root) && path.is_file()).then_some(path))
}

pub(super) fn parse_cache_target(target: &str) -> Option<&str> {
    let path = target.strip_prefix('/')?.split('?').next()?;
    if path.is_empty()
        || path.contains('\\')
        || path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return None;
    }
    if path == "nix-cache-info"
        || (path.ends_with(".narinfo") && !path.contains('/'))
        || path.starts_with("nar/")
        || path.starts_with("log/")
    {
        Some(path)
    } else {
        None
    }
}

fn write_headers(output: &mut impl Write, status: &str, length: u64) -> io::Result<()> {
    write!(
        output,
        "HTTP/1.1 {status}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
    )
}

fn write_missing(output: &mut impl Write, method: Method) -> io::Result<()> {
    write_headers(output, "404 Not Found", 10)?;
    if matches!(method, Method::Get) {
        output.write_all(b"not found\n")?;
    }
    Ok(())
}

fn reject(output: &mut impl Write, rejection: Rejection) -> io::Result<()> {
    write_headers(output, rejection.status(), 0)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "cache request deadline exceeded"))
}

struct Output<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
}

impl<'a> Output<'a> {
    fn new(stream: &'a mut TcpStream) -> Self {
        Self {
            stream,
            deadline: Instant::now() + RESPONSE_TIMEOUT,
        }
    }
}

impl Write for Output<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream
            .set_write_timeout(Some(remaining(self.deadline)?.min(WRITE_TIMEOUT)))?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
