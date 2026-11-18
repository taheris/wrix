#![cfg(test)]

use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}

fn connect(address: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
}

fn request(address: SocketAddr, message: &[u8]) {
    let mut stream = connect(address);
    stream.write_all(message).unwrap();
    let mut response = [0; 3];
    stream.read_exact(&mut response).unwrap();
    assert_eq!(&response, b"ok\n");
}

#[test]
fn unexpected_connections_preserve_dual_stack_controls_until_token_shutdown() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("control");
    let outside = temporary.path().join("outside");
    let output = temporary.path().join("output");
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let mut child = Owned(
        Command::new(env!("CARGO_BIN_EXE_wrix-native-test"))
            .args(["boundary", "fixture-serve"])
            .arg(&root)
            .arg(&outside)
            .arg(port.to_string())
            .stdout(fs::File::create(&output).unwrap())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let bytes = fs::read(&output).unwrap();
        if bytes.ends_with(b"\n") {
            let ready: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(ready["ready"], true);
            assert_eq!(ready["pid"], child.0.id());
            break;
        }
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(
            Instant::now() < deadline,
            "server did not become ready within five seconds"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    drop(connect(v4));
    let evidence = root.join("unexpected");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fs::read(&evidence).unwrap().ends_with(b"\n") {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let first: Value = serde_json::from_slice(&fs::read(&evidence).unwrap()).unwrap();
    assert_eq!(first["bytes"], 0);
    assert_eq!(first["peer"][0], "127.0.0.1");
    assert_eq!(first["peer"].as_array().unwrap().len(), 2);
    assert_eq!(
        fs::read(&outside).unwrap(),
        b"known-live-readable-service-control\n"
    );
    for address in [v4, v6] {
        request(address, b"probe\n");
    }
    request(v6, b"\x0b \tprobe\r\n\x0c");
    let mut unexpected = connect(v6);
    unexpected.write_all(b"bogus \r\n").unwrap();
    drop(unexpected);
    request(v6, b"probe\n");
    let entries: Vec<Value> = fs::read_to_string(&evidence)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1]["bytes"], 5);
    assert_eq!(entries[1]["peer"][0], "::1");
    assert_eq!(entries[1]["peer"].as_array().unwrap().len(), 4);
    assert!(child.0.try_wait().unwrap().is_none());
    let token = fs::read(root.join("token")).unwrap();
    assert_eq!(token.len(), 64);
    assert!(token.iter().all(u8::is_ascii_hexdigit));
    let stopped = Command::new(env!("CARGO_BIN_EXE_wrix-native-test"))
        .args(["boundary", "fixture-stop"])
        .arg(&root)
        .arg(port.to_string())
        .output()
        .unwrap();
    assert!(stopped.status.success(), "{stopped:?}");
    assert_eq!(stopped.stdout, b"");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "token shutdown exceeded five seconds"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!root.exists());
    assert!(!outside.exists());
    assert!(TcpStream::connect(v4).is_err());
    assert!(TcpStream::connect(v6).is_err());
}

#[test]
fn service_operation_refuses_an_unqualified_host_or_unknown_arguments() {
    for args in [
        vec!["boundary"],
        vec!["boundary", "unknown"],
        vec!["boundary", "serve", "extra"],
        vec!["boundary", "stop", "extra"],
        vec!["boundary", "boot-id", "extra"],
        vec!["boundary", "fixture-stop"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_wrix-native-test"))
            .args(args)
            .output()
            .unwrap();
        assert!(!result.status.success());
    }
    if !rustix::process::geteuid().is_root() {
        for operation in ["serve", "stop", "boot-id"] {
            let result = Command::new(env!("CARGO_BIN_EXE_wrix-native-test"))
                .args(["boundary", operation])
                .output()
                .unwrap();
            assert!(!result.status.success());
            assert!(
                String::from_utf8(result.stderr)
                    .unwrap()
                    .contains("require the native service owner")
            );
        }
    }
}
