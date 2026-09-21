use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn static_server_enforces_binary_cache_path_policy() -> TestResult {
    let fixture = tempfile::Builder::new().prefix("helper-server").tempdir()?;
    let cache_root = fixture.path().join("cache-root");
    fs::create_dir_all(cache_root.join("nar"))?;
    fs::create_dir_all(cache_root.join("log"))?;
    fs::write(
        cache_root.join("nix-cache-info"),
        "StoreDir: /nix/store\nWantMassQuery: 1\n",
    )?;
    fs::write(
        cache_root.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-demo.narinfo"),
        "StorePath: /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-demo\nURL: nar/demo.nar\n",
    )?;
    fs::write(cache_root.join("nar/demo.nar"), "nar payload\n")?;
    fs::write(cache_root.join("log/build.log"), "log payload\n")?;
    fs::write(cache_root.join("secret"), "secret\n")?;

    let port = available_loopback_port()?;
    let endpoint = format!("127.0.0.1:{port}");
    let _server = Server::start(&cache_root, &endpoint)?;

    let info = request(
        &endpoint,
        "GET /nix-cache-info HTTP/1.1\r\nHost: cache\r\n\r\n",
    )?;
    assert!(info.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(info.ends_with("StoreDir: /nix/store\nWantMassQuery: 1\n"));

    let head = request(
        &endpoint,
        "HEAD /aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-demo.narinfo HTTP/1.1\r\nHost: cache\r\n\r\n",
    )?;
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(head.ends_with("\r\n\r\n"));
    assert!(!head.contains("StorePath"));

    let nar = request(
        &endpoint,
        "GET /nar/demo.nar HTTP/1.1\r\nHost: cache\r\n\r\n",
    )?;
    assert!(nar.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(nar.ends_with("nar payload\n"));

    let log = request(
        &endpoint,
        "GET /log/build.log HTTP/1.1\r\nHost: cache\r\n\r\n",
    )?;
    assert!(log.starts_with("HTTP/1.1 200 OK\r\n"));

    let method = request(
        &endpoint,
        "POST /nar/demo.nar HTTP/1.1\r\nHost: cache\r\n\r\n",
    )?;
    assert!(method.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));

    for target in [
        "/",
        "/secret",
        "/nested/demo.narinfo",
        "/nar/../secret",
        "/nar//demo.nar",
    ] {
        let response = request(
            &endpoint,
            &format!("GET {target} HTTP/1.1\r\nHost: cache\r\n\r\n"),
        )?;
        assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(!response.contains("secret"));
    }

    Ok(())
}

#[test]
fn idle_clients_do_not_block_other_requests() -> TestResult {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("nix-cache-info"), "ready")?;
    let endpoint = format!("127.0.0.1:{}", available_loopback_port()?);
    let _server = Server::start(root.path(), &endpoint)?;
    let _idle = TcpStream::connect(&endpoint)?;
    thread::sleep(Duration::from_millis(50));
    assert!(request(&endpoint, "GET /nix-cache-info HTTP/1.1\r\n\r\n")?.ends_with("ready"));
    Ok(())
}

#[test]
fn request_headers_are_bounded_and_complete() -> TestResult {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("nix-cache-info"), "ready")?;
    let endpoint = format!("127.0.0.1:{}", available_loopback_port()?);
    let _server = Server::start(root.path(), &endpoint)?;
    let oversized = format!(
        "GET /nix-cache-info HTTP/1.1\r\nX-Large: {}\r\n\r\n",
        "a".repeat(9000)
    );
    assert!(request(&endpoint, &oversized)?.starts_with("HTTP/1.1 431"));
    assert!(request(&endpoint, "GET /nix-cache-info HTTP/1.1\r\n")?.starts_with("HTTP/1.1 400"));
    Ok(())
}

#[test]
fn disconnected_clients_do_not_terminate_service() -> TestResult {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("nix-cache-info"), "ready")?;
    let endpoint = format!("127.0.0.1:{}", available_loopback_port()?);
    let mut server = Server::start(root.path(), &endpoint)?;
    for _ in 0..64 {
        let stream = TcpStream::connect(&endpoint)?;
        socket2::SockRef::from(&stream).set_linger(Some(Duration::ZERO))?;
        drop(stream);
    }
    thread::sleep(Duration::from_millis(200));
    assert!(server.child.try_wait()?.is_none());
    assert!(request(&endpoint, "GET /nix-cache-info HTTP/1.1\r\n\r\n")?.ends_with("ready"));
    Ok(())
}

#[test]
fn partial_request_deadline_is_not_extended_by_trickling() -> TestResult {
    let root = tempfile::tempdir()?;
    let endpoint = format!("127.0.0.1:{}", available_loopback_port()?);
    let _server = Server::start(root.path(), &endpoint)?;
    let mut stream = TcpStream::connect(&endpoint)?;
    stream.set_read_timeout(Some(Duration::from_secs(7)))?;
    stream.write_all(b"GET /nix-cache-info HTTP/1.1\r\nX-Slow: ")?;
    let mut writer = stream.try_clone()?;
    let trickle = thread::spawn(move || {
        for _ in 0..8 {
            thread::sleep(Duration::from_secs(1));
            if writer.write_all(b"a").is_err() {
                break;
            }
        }
    });
    let start = Instant::now();
    let mut byte = [0];
    let read = stream.read(&mut byte);
    assert!(
        matches!(read, Ok(0))
            || matches!(read, Err(ref error) if error.kind() == io::ErrorKind::ConnectionReset),
        "{read:?}"
    );
    assert!(start.elapsed() < Duration::from_secs(7));
    trickle.join().unwrap();
    Ok(())
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "address-space limit requires Linux"
)]
fn head_reads_only_metadata_and_get_streams_large_files() -> TestResult {
    let root = tempfile::tempdir()?;
    fs::create_dir(root.path().join("nar"))?;
    let file = fs::File::create(root.path().join("nar/sparse"))?;
    file.set_len(64 * 1024 * 1024 * 1024)?;
    let endpoint = format!("127.0.0.1:{}", available_loopback_port()?);
    let _server = Server::start_limited(root.path(), &endpoint)?;
    let head = request(&endpoint, "HEAD /nar/sparse HTTP/1.1\r\n\r\n")?;
    assert!(head.contains("Content-Length: 68719476736\r\n"));
    assert!(head.ends_with("\r\n\r\n"));

    let mut file = fs::File::create(root.path().join("nar/large"))?;
    let chunk = vec![b'x'; 65536];
    for _ in 0..4096 {
        file.write_all(&chunk)?;
    }
    let mut stream = TcpStream::connect(&endpoint)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(b"GET /nar/large HTTP/1.1\r\n\r\n")?;
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" {
            break;
        }
        assert!(!line.is_empty());
        headers.push_str(&line);
    }
    assert!(headers.contains("Content-Length: 268435456\r\n"));
    let mut buffer = vec![0; 65536];
    for _ in 0..4096 {
        reader.read_exact(&mut buffer)?;
        assert_eq!(buffer, chunk);
    }
    assert_eq!(reader.read(&mut buffer)?, 0);
    Ok(())
}

struct Server {
    child: Child,
}

impl Server {
    fn start(cache_root: &Path, endpoint: &str) -> TestResult<Self> {
        Self::spawn(
            Command::new(env!("CARGO_BIN_EXE_wrix-cache-serve")),
            cache_root,
            endpoint,
        )
    }

    fn start_limited(cache_root: &Path, endpoint: &str) -> TestResult<Self> {
        let mut command = Command::new("bash");
        command.args([
            "-c",
            "ulimit -v 131072; exec \"$@\"",
            "cache-test",
            env!("CARGO_BIN_EXE_wrix-cache-serve"),
        ]);
        Self::spawn(command, cache_root, endpoint)
    }

    fn spawn(mut command: Command, cache_root: &Path, endpoint: &str) -> TestResult<Self> {
        let child = command
            .arg("--listen")
            .arg(endpoint)
            .arg(cache_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let server = Self { child };
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if request(
                endpoint,
                "GET /nix-cache-info HTTP/1.1\r\nHost: cache\r\n\r\n",
            )
            .is_ok()
            {
                return Ok(server);
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, "cache server did not start").into())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _kill = self.child.kill();
        let _wait = self.child.wait();
    }
}

fn request(endpoint: &str, request: &str) -> io::Result<String> {
    let mut stream = TcpStream::connect(endpoint)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(request.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = String::new();
    if let Err(error) = stream.read_to_string(&mut response) {
        // An early rejection can reset the connection while unread request bytes remain.
        if error.kind() != io::ErrorKind::ConnectionReset || response.is_empty() {
            return Err(error);
        }
    }
    Ok(response)
}

fn available_loopback_port() -> io::Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}
