use super::*;
use std::{
    fs,
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::Instant,
};
struct Tls {
    _root: tempfile::TempDir,
    ca: std::path::PathBuf,
    public: Public,
    child: Child,
}
impl Tls {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(root.path()).unwrap();
        let www = path.join("www");
        fs::create_dir(&www).unwrap();
        fs::write(www.join("nix-cache-info"), b"StoreDir: /nix/store\n").unwrap();
        let ca = path.join("ca.pem");
        let ca_key = path.join("ca-key.pem");
        let key = path.join("key.pem");
        let request = path.join("server.csr");
        let certificate = path.join("server.pem");
        let extensions = path.join("server.cnf");
        fs::write(&extensions, "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n").unwrap();
        openssl(
            Command::new("openssl")
                .args([
                    "req",
                    "-x509",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-sha256",
                    "-days",
                    "1",
                    "-subj",
                    "/CN=Private-Probe-CA",
                    "-out",
                ])
                .arg(&ca)
                .arg("-keyout")
                .arg(&ca_key),
        );
        openssl(
            Command::new("openssl")
                .args([
                    "req",
                    "-new",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-subj",
                    "/CN=localhost",
                    "-out",
                ])
                .arg(&request)
                .arg("-keyout")
                .arg(&key),
        );
        openssl(
            Command::new("openssl")
                .args(["x509", "-req", "-sha256", "-days", "1", "-in"])
                .arg(&request)
                .arg("-CA")
                .arg(&ca)
                .arg("-CAkey")
                .arg(&ca_key)
                .arg("-CAcreateserial")
                .arg("-extfile")
                .arg(&extensions)
                .arg("-out")
                .arg(&certificate),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let child = Command::new("openssl")
            .args([
                "s_server",
                "-quiet",
                "-WWW",
                "-accept",
                &address.to_string(),
                "-cert",
            ])
            .arg(&certificate)
            .arg("-key")
            .arg(&key)
            .current_dir(www)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let public = Public::parse(&format!(
            "https://localhost:{}/nix-cache-info",
            address.port()
        ))
        .unwrap();
        let mut fixture = Self {
            _root: root,
            ca,
            public,
            child,
        };
        let until = Instant::now() + Duration::from_secs(5);
        while TcpStream::connect(address).is_err() {
            assert!(fixture.child.try_wait().unwrap().is_none());
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(10));
        }
        fixture
    }
}
impl Drop for Tls {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
        }
        self.child.wait().unwrap();
    }
}
fn openssl(command: &mut Command) {
    let output = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((socket, _)) => return socket,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < until);
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error}"),
        }
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn expected() -> Hash {
    Hash::parse(&format!("{:x}", Sha256::digest(b"StoreDir: /nix/store\n"))).unwrap()
}
fn closed_private() -> Private {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Private::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    drop(listener);
    url
}
#[test]
fn actual_tls_public_bytes_are_verified_and_unavailable_private_service_is_denied() {
    let tls = Tls::new();
    runtime()
        .block_on(client(&tls.ca).unwrap().get(tls.public.0.clone()).send())
        .unwrap();
    let body = runtime()
        .block_on(observe(
            Mode::Fixed,
            &client(&tls.ca).unwrap(),
            &tls.public,
            &closed_private(),
            &expected(),
        ))
        .unwrap();
    assert_eq!(body, b"StoreDir: /nix/store\n");
}
#[test]
fn unrelated_ca_does_not_disable_certificate_authentication() {
    let server = Tls::new();
    let other = Tls::new();
    assert!(matches!(
        runtime().block_on(fetch(
            &client(&other.ca).unwrap(),
            &server.public.0,
            Duration::from_secs(2)
        )),
        Err(Error::Request)
    ));
}
#[test]
fn ordinary_derivation_detects_network_access_instead_of_claiming_isolation() {
    let tls = Tls::new();
    assert!(matches!(
        runtime().block_on(observe(
            Mode::Pure,
            &client(&tls.ca).unwrap(),
            &tls.public,
            &closed_private(),
            &expected()
        )),
        Err(Error::Ordinary)
    ));
}
#[test]
fn unexpected_public_payload_cannot_produce_a_fixed_output() {
    let tls = Tls::new();
    let wrong = Hash::parse(&"0".repeat(64)).unwrap();
    assert!(matches!(
        runtime().block_on(observe(
            Mode::Fixed,
            &client(&tls.ca).unwrap(),
            &tls.public,
            &closed_private(),
            &wrong
        )),
        Err(Error::Content)
    ));
}
#[test]
fn reachable_private_service_is_never_counted_as_a_successful_fence() {
    let tls = Tls::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let private = Private::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let thread = thread::spawn(move || {
        let mut socket = accept(&listener);
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = [0; 4096];
        assert!(socket.read(&mut request).unwrap() > 0);
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\nprivate-core-fixture").unwrap();
    });
    let result = runtime().block_on(observe(
        Mode::Fixed,
        &client(&tls.ca).unwrap(),
        &tls.public,
        &private,
        &expected(),
    ));
    assert!(matches!(result, Err(Error::Private)));
    thread.join().unwrap();
}
#[test]
fn response_prefix_preserves_the_original_4096_byte_read_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = thread::spawn(move || {
        let mut socket = accept(&listener);
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = [0; 4096];
        assert!(socket.read(&mut request).unwrap() > 0);
        let mut bytes =
            b"HTTP/1.1 200 OK\r\nContent-Length: 8192\r\nConnection: close\r\n\r\n".to_vec();
        bytes.extend(vec![b'x'; 8192]);
        socket.write_all(&bytes).unwrap();
    });
    let bytes = runtime()
        .block_on(fetch(
            &reqwest::Client::builder().no_proxy().build().unwrap(),
            &reqwest::Url::parse(&format!("http://{address}")).unwrap(),
            Duration::from_secs(2),
        ))
        .unwrap();
    assert_eq!(bytes, vec![b'x'; 4096]);
    thread.join().unwrap();
}
