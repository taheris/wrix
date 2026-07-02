use std::{
    fs,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn static_server_serves_only_binary_cache_paths_on_persisted_loopback_port() -> TestResult {
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

    assert!((21_000..=22_999).contains(&port));
    Ok(())
}

struct Server {
    child: Child,
}

impl Server {
    fn start(cache_root: &Path, endpoint: &str) -> TestResult<Self> {
        let child = Command::new(env!("CARGO_BIN_EXE_wrix-cache-serve"))
            .arg("--listen")
            .arg(endpoint)
            .arg(cache_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if request(
                endpoint,
                "GET /nix-cache-info HTTP/1.1\r\nHost: cache\r\n\r\n",
            )
            .is_ok()
            {
                return Ok(Self { child });
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
    stream.write_all(request.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn available_loopback_port() -> io::Result<u16> {
    for port in 21_000..=22_999 {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no free project-cache loopback port",
    ))
}
