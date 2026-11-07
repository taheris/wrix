//! Runs against the real Nix-packaged host/container clients and route fixture.
#![cfg(test)]
use std::{
    env, fs,
    io::Read,
    net::{Ipv4Addr, SocketAddrV4, TcpListener},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use wait_timeout::ChildExt;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    root: tempfile::TempDir,
    host: PathBuf,
    client: PathBuf,
    ip: PathBuf,
}

fn output(command: &mut Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if child
        .wait_timeout(Duration::from_secs(3))
        .unwrap()
        .is_none()
    {
        child.kill().unwrap();
        let result = child.wait_with_output().unwrap();
        panic!(
            "client timed out: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    child.wait_with_output().unwrap()
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: tempfile::Builder::new()
                .prefix("wrix-notify-")
                .tempdir_in("/tmp")
                .unwrap(),
            host: env::var_os("WRIX_TEST_NOTIFY_HOST").unwrap().into(),
            client: env::var_os("WRIX_TEST_NOTIFY_CLIENT").unwrap().into(),
            ip: env::var_os("WRIX_TEST_IP_COMMAND").unwrap().into(),
        }
    }

    fn calls(&self) -> PathBuf {
        self.root.path().join("ip-calls")
    }
    fn runtime(&self) -> PathBuf {
        self.root.path().join("runtime")
    }

    fn command(&self, program: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command.env_clear().args(args);
        for name in ["PATH", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE"] {
            if let Some(value) = env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("HOME", self.root.path())
            .env("XDG_RUNTIME_DIR", self.runtime())
            .env("WRIX_NOTIFY_TEST_IP_CALLS", self.calls());
        command
    }

    fn invoke(&self, command: &mut Command) -> Output {
        let result = output(command);
        assert!(
            result.status.success(),
            "client in {}: {}",
            self.root.path().display(),
            String::from_utf8_lossy(&result.stderr)
        );
        result
    }

    fn unix(&self, directory: &Path) -> UnixListener {
        assert!(directory.starts_with(self.root.path()));
        let path = directory.join("wrix/notify.sock");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    }
}

fn envelope(mut stream: impl Read) -> Value {
    let mut bytes = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut chunk = [0; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("notification envelope did not finish: {error}"),
        }
    }
    let payload = bytes.strip_suffix(b"\n").unwrap();
    assert!(!payload.contains(&b'\n'), "{bytes:?}");
    serde_json::from_slice(&bytes).unwrap()
}

fn unix_envelope(listener: &UnixListener) -> Value {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(true).unwrap();
                return envelope(stream);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("no Unix envelope: {error}"),
        }
    }
}

fn tcp_listener(port: u16) -> TcpListener {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

fn tcp_envelope(listener: &TcpListener) -> Value {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(true).unwrap();
                return envelope(stream);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("no TCP envelope: {error}"),
        }
    }
}

#[test]
#[ignore = "requires actual Nix-packaged notification clients"]
fn gateway_fixture_conforms_to_ip_route_interface() {
    let fixture = Fixture::new();
    let result = fixture.invoke(
        fixture
            .command(&fixture.ip, &["route"])
            .env("WRIX_NOTIFY_TEST_ROUTES", "default via 127.0.0.1 dev eth0"),
    );
    assert_eq!(result.stdout, b"default via 127.0.0.1 dev eth0\n");
    assert_eq!(fs::read_to_string(fixture.calls()).unwrap(), "route\n");
    assert_eq!(
        output(&mut fixture.command(&fixture.ip, &["unexpected"]))
            .status
            .code(),
        Some(64)
    );
}

#[test]
#[ignore = "requires actual Nix-packaged notification clients"]
fn harness_ignores_ambient_shell_startup_for_gateway_fixture() {
    let root = tempfile::tempdir().unwrap();
    let startup = root.path().join("startup");
    fs::write(
        &startup,
        "set -euo pipefail\nif [[ ${0##*/} == ip ]]; then exec sleep 30; fi\n",
    )
    .unwrap();
    let result = output(
        Command::new(env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "gateway_fixture_conforms_to_ip_route_interface",
            ])
            .env("BASH_ENV", startup),
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
#[ignore = "requires actual Nix-packaged notification clients"]
fn host_delivers_unix_envelope_and_ignores_tcp_endpoints() {
    let fixture = Fixture::new();
    let listener = fixture.unix(&fixture.runtime());
    for endpoint in ["1", "127.0.0.1:5959"] {
        let result = fixture.invoke(
            fixture
                .command(
                    &fixture.host,
                    &["Title \"quoted\"", "Message\nsecond line", "Ping"],
                )
                .env("WRIX_NOTIFY_TCP", endpoint)
                .env("WRIX_FOCUS_TARGET", "opaque:target"),
        );
        assert_eq!(result.stderr, b"");
        assert_eq!(
            unix_envelope(&listener),
            json!({"title":"Title \"quoted\"", "message":"Message\nsecond line", "sound":"Ping", "focus_target":"opaque:target"})
        );
        assert!(!fixture.calls().exists());
    }
}

#[test]
#[ignore = "requires actual Nix-packaged notification clients"]
fn host_uses_home_socket_when_runtime_directory_is_unset() {
    let fixture = Fixture::new();
    let listener = fixture.unix(&fixture.root.path().join(".local/share"));
    fixture.invoke(
        fixture
            .command(&fixture.host, &["Home", "Complete"])
            .env_remove("XDG_RUNTIME_DIR"),
    );
    assert_eq!(
        unix_envelope(&listener),
        json!({"title":"Home", "message":"Complete", "sound":""})
    );
}

#[test]
#[ignore = "requires actual Nix-packaged notification clients"]
fn host_missing_socket_is_best_effort() {
    let fixture = Fixture::new();
    let result = fixture.invoke(
        fixture
            .command(&fixture.host, &["Missing", "Complete"])
            .env("WRIX_NOTIFY_TCP", "1"),
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("wrix-notify: socket not found at"));
    assert!(!fixture.calls().exists());
}

#[test]
#[ignore = "requires actual Nix-packaged notification clients"]
fn host_unix_transport_failure_is_best_effort() -> Result {
    let fixture = Fixture::new();
    let path = fixture.runtime().join("wrix/notify.sock");
    fs::create_dir_all(path.parent().unwrap())?;
    let socket = rustix::net::socket(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        None,
    )?;
    rustix::net::bind(&socket, &rustix::net::SocketAddrUnix::new(&path)?)?;
    let result = fixture.invoke(&mut fixture.command(&fixture.host, &["Unavailable", "Complete"]));
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("wrix-notify: Unix socket send failed")
    );
    Ok(())
}

#[test]
#[ignore = "requires actual Nix-packaged notification clients"]
fn container_discovers_first_default_gateway_with_injected_ip_command() {
    let fixture = Fixture::new();
    let listener = tcp_listener(5959);
    fixture.invoke(fixture.command(&fixture.client, &["Gateway", "Complete"]).env("WRIX_NOTIFY_TCP", "1").env("WRIX_NOTIFY_TEST_ROUTES", "10.0.0.0/24 dev eth0\ndefault via 127.0.0.1 dev eth0\ndefault via 192.0.2.1 dev eth1"));
    assert_eq!(
        tcp_envelope(&listener),
        json!({"title":"Gateway", "message":"Complete", "sound":""})
    );
    assert_eq!(fs::read_to_string(fixture.calls()).unwrap(), "route\n");
}

#[test]
#[ignore = "requires actual Nix-packaged notification clients"]
fn container_explicit_tcp_endpoint_does_not_discover_gateway() {
    let fixture = Fixture::new();
    let listener = tcp_listener(0);
    fixture.invoke(
        fixture
            .command(&fixture.client, &["Explicit", "Complete"])
            .env(
                "WRIX_NOTIFY_TCP",
                listener.local_addr().unwrap().to_string(),
            ),
    );
    assert_eq!(
        tcp_envelope(&listener),
        json!({"title":"Explicit", "message":"Complete", "sound":""})
    );
    assert!(!fixture.calls().exists());
}

#[test]
#[ignore = "requires actual Nix-packaged notification clients"]
fn container_missing_gateway_is_best_effort() {
    let fixture = Fixture::new();
    let result = fixture.invoke(
        fixture
            .command(&fixture.client, &["Missing gateway", "Complete"])
            .env("WRIX_NOTIFY_TCP", "1"),
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("wrix-notify: invalid TCP endpoint: 1")
    );
    assert_eq!(fs::read_to_string(fixture.calls()).unwrap(), "route\n");
}

#[test]
#[ignore = "requires actual Nix-packaged notification clients"]
fn container_invalid_tcp_endpoint_is_best_effort() {
    let fixture = Fixture::new();
    let result = fixture.invoke(
        fixture
            .command(&fixture.client, &["Invalid", "Complete"])
            .env("WRIX_NOTIFY_TCP", "invalid"),
    );
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("wrix-notify: invalid TCP endpoint: invalid")
    );
    assert!(!fixture.calls().exists());
}

#[test]
#[ignore = "requires actual Nix-packaged notification clients"]
fn container_tcp_transport_failure_is_best_effort() -> Result {
    let fixture = Fixture::new();
    let socket = rustix::net::socket(
        rustix::net::AddressFamily::INET,
        rustix::net::SocketType::STREAM,
        Some(rustix::net::ipproto::TCP),
    )?;
    rustix::net::bind(&socket, &SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
    let address = SocketAddrV4::try_from(rustix::net::getsockname(&socket)?)?;
    let result = fixture.invoke(
        fixture
            .command(&fixture.client, &["Unavailable", "Complete"])
            .env("WRIX_NOTIFY_TCP", address.to_string()),
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("wrix-notify: TCP send failed"));
    assert!(!fixture.calls().exists());
    Ok(())
}
