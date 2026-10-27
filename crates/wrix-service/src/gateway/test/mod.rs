use super::*;
mod stream;
use std::{
    num::NonZeroU16,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
    time::timeout,
};

pub struct Fixture {
    pub data: SocketAddr,
    pub management: management::Client,
    management_address: SocketAddr,
    pub network: Arc<Network>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<(), Error>>,
    upstream: tokio::task::JoinHandle<()>,
}
pub struct Network {
    address: SocketAddr,
    answers: Mutex<Vec<SocketAddr>>,
    pub connections: AtomicUsize,
    pub pending: tokio::sync::Notify,
    dial_pending: AtomicBool,
    dialing: tokio::sync::Notify,
}
impl transport::Network for Network {
    fn resolve<'a>(
        &'a self,
        host: &'a policy::Host,
        _port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, Error>> + Send + 'a>> {
        Box::pin(async move {
            if host.as_str() == "pending.example" {
                self.pending.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(self.answers.lock().unwrap().clone())
        })
    }
    fn connect(
        &self,
        address: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<TcpStream, Error>> + Send + '_>> {
        Box::pin(async move {
            assert_eq!(address, "93.184.216.34:80".parse::<SocketAddr>().unwrap());
            if self.dial_pending.load(Ordering::SeqCst) {
                self.dialing.notify_one();
                std::future::pending::<()>().await;
            }
            self.connections.fetch_add(1, Ordering::SeqCst);
            Ok(TcpStream::connect(self.address).await?)
        })
    }
}
impl Fixture {
    pub async fn start() -> Self {
        Self::capacity(32).await
    }
    async fn capacity(capacity: usize) -> Self {
        Self::on_listener(TcpListener::bind("127.0.0.1:0").await.unwrap(), capacity).await
    }
    pub async fn on_listener(data: TcpListener, capacity: usize) -> Self {
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let network = Arc::new(Network {
            address: origin.local_addr().unwrap(),
            answers: Mutex::new(vec!["93.184.216.34:80".parse().unwrap()]),
            connections: AtomicUsize::new(0),
            pending: tokio::sync::Notify::new(),
            dial_pending: AtomicBool::new(false),
            dialing: tokio::sync::Notify::new(),
        });
        let upstream = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = origin.accept() => {
                        let (mut stream, _) = accepted.unwrap();
                        tasks.spawn(async move {
                            let bytes = header_bytes(&mut stream).await;
                            if bytes.starts_with(b"GET ") || bytes.starts_with(b"POST ") {
                                let header = std::str::from_utf8(&bytes).unwrap();
                                assert!(!header.to_ascii_lowercase().contains("proxy-authorization"));
                                if header.starts_with("GET /slow ") {
                                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000000\r\n\r\nhello").await.unwrap();
                                    let mut byte = [0];
                                    assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
                                } else if header.starts_with("GET /large ") {
                                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\n\r\n").await.unwrap();
                                    for _ in 0..128 { stream.write_all(&[b'x';8192]).await.unwrap(); }
                                } else if header.starts_with("POST ") {
                                    let mut byte = [0];
                                    while stream.read(&mut byte).await.unwrap() != 0 {}
                                } else {
                                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await.unwrap();
                                }
                            } else {
                                stream.write_all(&bytes).await.unwrap();
                                let (mut read, mut write) = stream.into_split();
                                if let Err(error) = tokio::io::copy(&mut read, &mut write).await { assert!(matches!(error.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe)); }
                            }
                        });
                    }
                    Some(result) = tasks.join_next() => { result.unwrap(); }
                }
            }
        });
        let address = data.local_addr().unwrap();
        let management = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let private = management.local_addr().unwrap();
        let key = Secret::generate().unwrap();
        let client = management::Client::new(private, key.clone());
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(run_network(
            data,
            management,
            key,
            vec![private, address],
            NonZeroUsize::new(capacity).unwrap(),
            network.clone(),
            async {
                stopped.await.unwrap();
            },
        ));
        Self {
            data: address,
            management: client,
            management_address: private,
            network,
            stop: Some(stop),
            task,
            upstream,
        }
    }
    pub async fn grant(&self, secret: &Secret) -> management::Attachment {
        self.management
            .attach(secret.clone(), policy())
            .await
            .unwrap()
    }
    pub async fn connect(&self) -> TcpStream {
        TcpStream::connect(self.data).await.unwrap()
    }
    pub async fn finish(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        (&mut self.task).await.unwrap().unwrap();
        self.upstream.abort();
        match (&mut self.upstream).await {
            Err(error) if error.is_cancelled() => (),
            other => panic!("unexpected origin completion: {other:?}"),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
        self.upstream.abort();
    }
}
pub fn policy() -> policy::Policy {
    policy::Policy::new(
        ["allowed.example", "pending.example"]
            .into_iter()
            .map(|host| {
                policy::Rule::new(
                    policy::Host::parse(host).unwrap(),
                    vec![NonZeroU16::new(80).unwrap()],
                )
                .unwrap()
            })
            .collect(),
    )
    .unwrap()
}
pub async fn header(socket: &mut TcpStream) -> String {
    String::from_utf8(header_bytes(socket).await).unwrap()
}
async fn header_bytes(socket: &mut TcpStream) -> Vec<u8> {
    timeout(Duration::from_secs(3), async {
        let mut result = Vec::new();
        loop {
            let mut byte = [0];
            let n = socket.read(&mut byte).await.unwrap();
            if n == 0 {
                break;
            }
            result.push(byte[0]);
            assert!(result.len() < 32768);
            if result.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        result
    })
    .await
    .unwrap()
}
pub async fn request(
    socket: &mut TcpStream,
    secret: &Secret,
    host: &str,
    method: &str,
    path: &str,
) -> String {
    let target = if method == "CONNECT" {
        format!("{host}:80")
    } else {
        format!("http://{host}{path}")
    };
    let authority = if method == "CONNECT" {
        format!("{host}:80")
    } else {
        host.to_owned()
    };
    socket.write_all(format!("{method} {target} HTTP/1.1\r\nHost: {authority}\r\nProxy-Authorization: Bearer {}\r\n\r\n", secret.expose()).as_bytes()).await.unwrap();
    header(socket).await
}
pub async fn get(socket: &mut TcpStream, secret: &Secret) {
    assert!(
        request(socket, secret, "allowed.example", "GET", "/")
            .await
            .starts_with("HTTP/1.1 200 ")
    );
    let mut body = [0; 5];
    socket.read_exact(&mut body).await.unwrap();
    assert_eq!(&body, b"hello");
}
pub async fn eof(socket: &mut TcpStream) {
    timeout(Duration::from_secs(3), async {
        let mut byte = [0];
        match socket.read(&mut byte).await {
            Ok(0) => (),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                ) => {}
            other => panic!("expected closed stream, got {other:?}"),
        }
    })
    .await
    .unwrap();
}

pub async fn revoked(socket: &mut TcpStream) {
    timeout(Duration::from_secs(3), async {
        let mut data = Vec::new();
        match socket.take(4096).read_to_end(&mut data).await {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => (),
            Err(error) => panic!("unexpected read: {error}"),
        }
        assert!(data.len() < 4096);
        assert!(
            data.is_empty() || data.starts_with(b"HTTP/1.1 407 "),
            "{data:?}"
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn external_numeric_dial_fixture_preserves_system_socket_contract() {
    let fixture = Fixture::start().await;
    let mut socket =
        transport::Network::connect(&*fixture.network, "93.184.216.34:80".parse().unwrap())
            .await
            .unwrap();
    socket
        .write_all(b"GET / HTTP/1.1\r\nHost: allowed.example\r\n\r\n")
        .await
        .unwrap();
    assert!(header(&mut socket).await.starts_with("HTTP/1.1 200 "));
    let mut body = [0; 5];
    socket.read_exact(&mut body).await.unwrap();
    assert_eq!(&body, b"hello");
    eof(&mut socket).await;
    fixture.finish().await;
}

#[tokio::test]
async fn keepalive_rechecks_exact_policy_without_forwarding_credentials() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let _grant = fixture.grant(&secret).await;
    let mut socket = fixture.connect().await;
    get(&mut socket, &secret).await;
    assert!(
        request(&mut socket, &secret, "denied.example", "GET", "/")
            .await
            .starts_with("HTTP/1.1 403 ")
    );
    get(&mut socket, &secret).await;
    assert_eq!(fixture.network.connections.load(Ordering::SeqCst), 2);
    fixture.finish().await;
}

#[tokio::test]
async fn connect_is_opaque_and_revocation_closes_tunnel_not_peer() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let grant = fixture.grant(&secret).await;
    let peer = Secret::generate().unwrap();
    let _peer_grant = fixture.grant(&peer).await;
    let mut socket = fixture.connect().await;
    assert!(
        request(&mut socket, &secret, "allowed.example", "CONNECT", "")
            .await
            .starts_with("HTTP/1.1 200 ")
    );
    socket.write_all(b"opaque\r\n\r\n").await.unwrap();
    let mut bytes = [0; 10];
    socket.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"opaque\r\n\r\n");
    drop(grant);
    eof(&mut socket).await;
    let mut peer_socket = fixture.connect().await;
    get(&mut peer_socket, &peer).await;
    let mut revoked = fixture.connect().await;
    assert!(
        request(&mut revoked, &secret, "allowed.example", "GET", "/")
            .await
            .starts_with("HTTP/1.1 407 ")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn revoked_credentials_cannot_be_reinstalled_in_same_generation() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    drop(fixture.grant(&secret).await);
    for _ in 0..100 {
        if fixture.management.status().await.unwrap().grants == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(fixture.management.attach(secret, policy()).await.is_err());
    fixture.finish().await;
}

#[tokio::test]
async fn native_readiness_requires_private_key_and_positive_negative_admission() {
    let fixture = Fixture::start().await;
    assert_eq!(
        fixture
            .management
            .verify(fixture.data)
            .await
            .unwrap()
            .version,
        management::VERSION
    );
    let secret = Secret::generate().unwrap();
    let _grant = fixture.grant(&secret).await;
    let wrong = management::Client::new(fixture.management_address, secret.clone());
    assert!(wrong.status().await.is_err());
    let mut traffic = fixture.connect().await;
    assert!(
        request(&mut traffic, &secret, "probe.wrix.invalid", "GET", "/")
            .await
            .starts_with("HTTP/1.1 403 ")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn dns_rebinding_and_mixed_private_answers_never_reach_numeric_dialer() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let _grant = fixture.grant(&secret).await;
    get(&mut fixture.connect().await, &secret).await;
    fixture
        .network
        .answers
        .lock()
        .unwrap()
        .push("127.0.0.1:80".parse().unwrap());
    let mut socket = fixture.connect().await;
    assert!(
        request(&mut socket, &secret, "allowed.example", "GET", "/")
            .await
            .starts_with("HTTP/1.1 403 ")
    );
    assert_eq!(fixture.network.connections.load(Ordering::SeqCst), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn authority_and_framing_confusion_are_rejected_before_upstream() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let _grant = fixture.grant(&secret).await;
    for (target, extra) in [
        ("http://allowed.example/", "Host: denied.example\r\n"),
        (
            "http://allowed.example/",
            "Host: allowed.example\r\nContent-Length: 1\r\nContent-Length: 2\r\n",
        ),
        (
            "http://allowed.example/",
            "Host: allowed.example\r\nConnection: host\r\n",
        ),
        (
            "http://allowed.example@denied.example/",
            "Host: denied.example\r\n",
        ),
    ] {
        let mut socket = fixture.connect().await;
        socket.write_all(format!("GET {target} HTTP/1.1\r\n{extra}Proxy-Authorization: Bearer {}\r\n\r\n0\r\n\r\n",secret.expose()).as_bytes()).await.unwrap();
        let response = header(&mut socket).await;
        assert!(response.starts_with("HTTP/1.1 400 "), "{response}");
    }
    assert_eq!(fixture.network.connections.load(Ordering::SeqCst), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn owner_loss_cancels_response_body_after_headers() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let grant = fixture.grant(&secret).await;
    let mut socket = fixture.connect().await;
    assert!(
        request(&mut socket, &secret, "allowed.example", "GET", "/slow")
            .await
            .starts_with("HTTP/1.1 200 ")
    );
    let mut body = [0; 5];
    socket.read_exact(&mut body).await.unwrap();
    assert_eq!(&body, b"hello");
    drop(grant);
    eof(&mut socket).await;
    fixture.finish().await;
}

#[tokio::test]
async fn owner_loss_cancels_pending_resolution_before_connect() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let grant = fixture.grant(&secret).await;
    let mut socket = fixture.connect().await;
    socket.write_all(format!("CONNECT pending.example:80 HTTP/1.1\r\nHost: pending.example:80\r\nProxy-Authorization: Bearer {}\r\n\r\n",secret.expose()).as_bytes()).await.unwrap();
    fixture.network.pending.notified().await;
    drop(grant);
    revoked(&mut socket).await;
    assert_eq!(fixture.network.connections.load(Ordering::SeqCst), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn draining_preserves_old_keepalive_but_rejects_new_connections() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let _grant = fixture.grant(&secret).await;
    let mut socket = fixture.connect().await;
    get(&mut socket, &secret).await;
    let status = fixture.management.drain(true).await.unwrap();
    assert_eq!(status.connections, 1);
    get(&mut socket, &secret).await;
    eof(&mut fixture.connect().await).await;
    fixture.finish().await;
}

#[test]
fn shared_baseline_blocks_special_ipv4_and_all_ipv6() {
    let baseline = policy::Baseline::load().unwrap();
    for ip in [
        "0.1.2.3",
        "10.0.0.1",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "192.0.0.9",
        "192.0.2.1",
        "192.88.99.1",
        "192.168.0.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "255.255.255.255",
    ] {
        assert!(!baseline.permits(format!("{ip}:443").parse().unwrap()));
    }
    assert!(!baseline.permits("[::ffff:93.184.216.34]:80".parse().unwrap()));
    assert!(baseline.permits("93.184.216.34:443".parse().unwrap()));
}
