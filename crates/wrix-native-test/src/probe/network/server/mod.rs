use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const BODY: &[u8] = b"private-core-fixture";
#[must_use]
pub struct Server {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<io::Result<()>>>,
    address: SocketAddr,
}
fn respond(mut stream: TcpStream) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut request = Vec::new();
    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "private fixture header deadline")
            })?;
        stream.set_read_timeout(Some(remaining))?;
        let mut bytes = [0; 256];
        let count = stream.read(&mut bytes)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "private fixture incomplete header",
            ));
        }
        if request.len() + count > 2048 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "private fixture oversized header",
            ));
        }
        request.extend_from_slice(&bytes[..count]);
    }
    if !request.starts_with(b"GET ") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private fixture requires GET",
        ));
    }
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(
        stream,
        "HTTP/1.0 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        BODY.len()
    )?;
    stream.write_all(BODY)?;
    Ok(())
}
impl Server {
    pub fn start(address: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("owned-private-service".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => respond(stream)?,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
            address,
        })
    }
    pub const fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn finish(mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        let thread = self
            .thread
            .take()
            .ok_or_else(|| io::Error::other("missing owned private fixture handle"))?;
        thread
            .join()
            .map_err(|_| io::Error::other("owned private fixture thread panicked"))?
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let result = thread
                .join()
                .map_err(|_| io::Error::other("owned private fixture thread panicked"))
                .and_then(|result| result);
            if let Err(error) = result {
                let _ = writeln!(
                    io::stderr().lock(),
                    "owned private fixture fallback cleanup failed: {error}"
                );
            }
        }
    }
}
#[cfg(test)]
mod test;
