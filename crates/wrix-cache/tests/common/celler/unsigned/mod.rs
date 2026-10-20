use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub struct Server {
    pub endpoint: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    pub fn start(hash: &str, signed_info: &str, nar: &[u8]) -> Self {
        assert!(signed_info.lines().any(|line| line.starts_with("Sig: ")));
        let url = signed_info
            .lines()
            .find_map(|line| line.strip_prefix("URL: "))
            .unwrap();
        assert!(!url.starts_with('/') && !url.contains(".."));
        let info = signed_info
            .lines()
            .filter(|line| !line.starts_with("Sig: "))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let assets = HashMap::from([
            (
                String::from("/nix-cache-info"),
                b"StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 40\n".to_vec(),
            ),
            (format!("/{hash}.narinfo"), info.into_bytes()),
            (format!("/{url}"), nar.to_vec()),
        ]);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => respond(stream, &assets),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("unsigned control accept failed: {error}"),
                }
            }
        });
        Self {
            endpoint,
            stop,
            worker: Some(worker),
        }
    }
}

fn respond(mut stream: TcpStream, assets: &HashMap<String, Vec<u8>>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
        assert!(header.len() < 16384);
    }
    let request = std::str::from_utf8(&header).unwrap();
    let mut words = request.lines().next().unwrap().split_whitespace();
    let method = words.next().unwrap();
    let path = words.next().unwrap();
    let (status, body): (_, &[u8]) = match (method, assets.get(path)) {
        ("GET" | "HEAD", Some(body)) => (200, body),
        ("GET" | "HEAD", None) => (404, b"not found"),
        _ => (405, b"method not allowed"),
    };
    write!(
        stream,
        "HTTP/1.1 {status} Control\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    if method != "HEAD" {
        stream.write_all(body).unwrap();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
