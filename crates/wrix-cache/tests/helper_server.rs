use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn server_fixture_keeps_ephemeral_listeners_isolated() -> TestResult {
    let first_root = tempfile::tempdir()?;
    let second_root = tempfile::tempdir()?;
    fs::write(first_root.path().join("nix-cache-info"), "first cache")?;
    fs::write(second_root.path().join("nix-cache-info"), "second cache")?;
    let (_first, first_endpoint) = Server::start(first_root.path())?;
    let (_second, second_endpoint) = Server::start(second_root.path())?;
    assert_ne!(first_endpoint, second_endpoint);
    for (endpoint, body) in [
        (first_endpoint, "first cache"),
        (second_endpoint, "second cache"),
    ] {
        let response = request(endpoint, "GET /nix-cache-info HTTP/1.1\r\n\r\n")?;
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with(body));
    }
    Ok(())
}

#[test]
fn server_fixture_reports_bind_failure_without_contacting_listener() -> TestResult {
    let root = tempfile::tempdir()?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let result = Server::spawn(
        Command::new(env!("CARGO_BIN_EXE_wrix-cache-serve")),
        root.path(),
        listener.local_addr()?,
    );
    let Err(error) = result else {
        panic!("cache server started on an occupied port");
    };
    let diagnostic = error.to_string();
    assert!(
        diagnostic.contains("cache server exited during startup"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("wrix-cache-serve: cache helper I/O failed"),
        "{diagnostic}"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    Ok(())
}

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

    let (_server, endpoint) = Server::start(&cache_root)?;

    let info = request(
        endpoint,
        "GET /nix-cache-info HTTP/1.1\r\nHost: cache\r\n\r\n",
    )?;
    assert!(info.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(info.ends_with("StoreDir: /nix/store\nWantMassQuery: 1\n"));

    let head = request(
        endpoint,
        "HEAD /aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-demo.narinfo HTTP/1.1\r\nHost: cache\r\n\r\n",
    )?;
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(head.ends_with("\r\n\r\n"));
    assert!(!head.contains("StorePath"));

    let nar = request(
        endpoint,
        "GET /nar/demo.nar HTTP/1.1\r\nHost: cache\r\n\r\n",
    )?;
    assert!(nar.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(nar.ends_with("nar payload\n"));

    let log = request(
        endpoint,
        "GET /log/build.log HTTP/1.1\r\nHost: cache\r\n\r\n",
    )?;
    assert!(log.starts_with("HTTP/1.1 200 OK\r\n"));

    let method = request(
        endpoint,
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
            endpoint,
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
    let (_server, endpoint) = Server::start(root.path())?;
    let _idle = TcpStream::connect(endpoint)?;
    thread::sleep(Duration::from_millis(50));
    assert!(request(endpoint, "GET /nix-cache-info HTTP/1.1\r\n\r\n")?.ends_with("ready"));
    Ok(())
}

#[test]
fn request_headers_are_bounded_and_complete() -> TestResult {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("nix-cache-info"), "ready")?;
    let (_server, endpoint) = Server::start(root.path())?;
    let oversized = format!(
        "GET /nix-cache-info HTTP/1.1\r\nX-Large: {}\r\n\r\n",
        "a".repeat(9000)
    );
    assert!(request(endpoint, &oversized)?.starts_with("HTTP/1.1 431"));
    assert!(request(endpoint, "GET /nix-cache-info HTTP/1.1\r\n")?.starts_with("HTTP/1.1 400"));
    Ok(())
}

#[test]
fn disconnected_clients_do_not_terminate_service() -> TestResult {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("nix-cache-info"), "ready")?;
    let (mut server, endpoint) = Server::start(root.path())?;
    for _ in 0..64 {
        let stream = TcpStream::connect(endpoint)?;
        socket2::SockRef::from(&stream).set_linger(Some(Duration::ZERO))?;
        drop(stream);
    }
    thread::sleep(Duration::from_millis(200));
    assert!(server.child.try_wait()?.is_none());
    assert!(request(endpoint, "GET /nix-cache-info HTTP/1.1\r\n\r\n")?.ends_with("ready"));
    Ok(())
}

#[test]
fn partial_request_deadline_is_not_extended_by_trickling() -> TestResult {
    let root = tempfile::tempdir()?;
    let (_server, endpoint) = Server::start(root.path())?;
    let mut stream = TcpStream::connect(endpoint)?;
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
    let (_server, endpoint) = Server::start_limited(root.path())?;
    let head = request(endpoint, "HEAD /nar/sparse HTTP/1.1\r\n\r\n")?;
    assert!(head.contains("Content-Length: 68719476736\r\n"));
    assert!(head.ends_with("\r\n\r\n"));

    let mut file = fs::File::create(root.path().join("nar/large"))?;
    let chunk = vec![b'x'; 65536];
    for _ in 0..4096 {
        file.write_all(&chunk)?;
    }
    let mut stream = TcpStream::connect(endpoint)?;
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
        assert_ne!(line, "");
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
    diagnostics: tempfile::NamedTempFile,
}

impl Server {
    fn start(cache_root: &Path) -> TestResult<(Self, SocketAddr)> {
        Self::spawn(
            Command::new(env!("CARGO_BIN_EXE_wrix-cache-serve")),
            cache_root,
            "127.0.0.1:0".parse()?,
        )
    }

    fn start_limited(cache_root: &Path) -> TestResult<(Self, SocketAddr)> {
        let mut command = Command::new("bash");
        command.args([
            "-c",
            "set -euo pipefail; ulimit -v 131072; exec \"$@\"",
            "cache-test",
            env!("CARGO_BIN_EXE_wrix-cache-serve"),
        ]);
        Self::spawn(command, cache_root, "127.0.0.1:0".parse()?)
    }

    fn spawn(
        mut command: Command,
        cache_root: &Path,
        listen: SocketAddr,
    ) -> TestResult<(Self, SocketAddr)> {
        let diagnostics = tempfile::NamedTempFile::new()?;
        let child = command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").ok_or("PATH is missing")?)
            .env("LC_ALL", "C")
            .env("NO_COLOR", "1")
            .arg("--listen")
            .arg(listen.to_string())
            .arg(cache_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(diagnostics.reopen()?)
            .spawn()?;
        let mut server = Self { child, diagnostics };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = server.child.try_wait()? {
                let diagnostics = fs::read_to_string(server.diagnostics.path())?;
                return Err(
                    format!("cache server exited during startup: {status}: {diagnostics}").into(),
                );
            }
            let diagnostics = fs::read_to_string(server.diagnostics.path())?;
            for line in diagnostics.lines() {
                if let Some((_, address)) = line.split_once("cache server listening address=") {
                    let endpoint: SocketAddr = address.parse()?;
                    assert_eq!(endpoint.ip(), listen.ip());
                    assert_ne!(endpoint.port(), 0);
                    return Ok((server, endpoint));
                }
            }
            if Instant::now() >= deadline {
                return Err(format!("cache server did not start: {diagnostics}").into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _kill = self.child.kill();
        let _wait = self.child.wait();
    }
}

fn request(endpoint: SocketAddr, request: &str) -> io::Result<String> {
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
