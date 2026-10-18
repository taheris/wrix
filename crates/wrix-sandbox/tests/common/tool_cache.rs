use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use super::TestResult;

pub fn success(command: &mut Command) -> TestResult<Output> {
    let output = command.output()?;
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

pub fn link_home(root: &Path, home: &Path) -> TestResult {
    success(
        Command::new("bash")
            .arg("-c")
            .arg(concat!(
                include_str!("../../../../lib/sandbox/tool-cache.sh"),
                "\nwrix_link_tool_caches \"$1\" \"$2\"\n"
            ))
            .arg("tool-cache")
            .arg(root)
            .arg(home),
    )?;
    Ok(())
}

pub struct HttpFixture {
    pub url: String,
    stop: Arc<AtomicBool>,
    server: Option<JoinHandle<()>>,
}

impl HttpFixture {
    pub fn start(files: BTreeMap<String, Vec<u8>>) -> TestResult<Self> {
        Self::start_with(|_| files)
    }

    pub fn start_with(files: impl FnOnce(&str) -> BTreeMap<String, Vec<u8>>) -> TestResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = format!("http://{}", listener.local_addr()?);
        let files = files(&url);
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let server = thread::spawn(move || {
            while !server_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        let mut reader = BufReader::new(&stream);
                        let mut request = String::new();
                        reader.read_line(&mut request).unwrap();
                        let mut header = String::new();
                        loop {
                            header.clear();
                            let count = reader.read_line(&mut header).unwrap();
                            if count == 0 || header == "\r\n" {
                                break;
                            }
                        }
                        let path = request.split_whitespace().nth(1).unwrap();
                        let (status, body) = files
                            .get(path)
                            .map_or(("404 Not Found", &b"missing"[..]), |body| {
                                ("200 OK", body.as_slice())
                            });
                        write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/html\r\nCache-Control: max-age=3600\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                        stream.write_all(body).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("HTTP fixture: {error}"),
                }
            }
        });
        Ok(Self {
            url,
            stop,
            server: Some(server),
        })
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            server.join().unwrap();
        }
    }
}

#[test]
fn http_fixture_serves_exact_bytes_and_rejects_unknown_paths() -> TestResult {
    let server = HttpFixture::start(BTreeMap::from([("/payload".into(), b"bytes".to_vec())]))?;
    for (path, expected) in [("/payload", "200 OK\r\n"), ("/absent", "404 Not Found\r\n")] {
        let mut stream = TcpStream::connect(server.url.trim_start_matches("http://"))?;
        write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n")?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        assert!(response.contains(expected));
        assert!(response.ends_with(if path == "/payload" {
            "bytes"
        } else {
            "missing"
        }));
    }
    Ok(())
}
