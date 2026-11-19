//! Known-live loopback observations; not service containment or host-store access.
mod control;

use serde_json::json;
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: boundary serve|stop|boot-id | boundary fixture-serve ROOT OUTSIDE PORT | boundary fixture-stop ROOT PORT
    Arguments,
    /// boundary shutdown token is not a private server token
    Token,
    /// boundary shutdown acknowledgment is not exactly ok followed by a newline
    Response,
    /// boundary controls require the native service owner, not an agent image
    Service,
    /// boundary I/O failed: {0}
    Io(#[from] io::Error),
    /// boundary evidence failed: {0}
    Json(#[from] serde_json::Error),
}

pub struct Server {
    root: PathBuf,
    outside: PathBuf,
    token: Vec<u8>,
    listeners: [TcpListener; 2],
}

fn private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

const fn whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn trim(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|byte| whitespace(*byte)) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(|byte| whitespace(*byte)) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

impl Server {
    pub fn bind(root: &Path, outside: &Path, port: u16) -> Result<Self, Error> {
        if !root.is_absolute() || !outside.is_absolute() || outside.starts_with(root) {
            return Err(Error::Arguments);
        }
        fs::DirBuilder::new().mode(0o700).create(root)?;
        let mut random = [0; 32];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let hex = b"0123456789abcdef";
        let token: Vec<u8> = random
            .into_iter()
            .flat_map(|byte| [hex[usize::from(byte >> 4)], hex[usize::from(byte & 15)]])
            .collect();
        private_file(&root.join("token"))?.write_all(&token)?;
        private_file(&root.join("unexpected"))?;
        let mut marker = private_file(outside)?;
        marker.write_all(b"known-live-readable-service-control\n")?;
        marker.set_permissions(fs::Permissions::from_mode(0o644))?;
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        let v6 = TcpListener::bind((Ipv6Addr::LOCALHOST, v4.local_addr()?.port()))?;
        v4.set_nonblocking(true)?;
        v6.set_nonblocking(true)?;
        Ok(Self {
            root: root.to_owned(),
            outside: outside.to_owned(),
            token,
            listeners: [v4, v6],
        })
    }

    pub fn port(&self) -> Result<u16, Error> {
        Ok(self.listeners[0].local_addr()?.port())
    }

    fn connection(&self, mut connection: TcpStream, peer: SocketAddr) -> Result<bool, Error> {
        // Darwin inherits the listener's nonblocking flag; socket timeouts alone
        // do not restore the original bounded blocking protocol.
        connection.set_nonblocking(false)?;
        connection.set_read_timeout(Some(Duration::from_secs(3)))?;
        connection.set_write_timeout(Some(Duration::from_secs(3)))?;
        let mut bytes = [0; 128];
        let count = connection.read(&mut bytes)?;
        let message = trim(&bytes[..count]);
        let stopping = message == self.token;
        if !stopping && message != b"probe" {
            let address = match peer {
                SocketAddr::V4(peer) => json!([peer.ip().to_string(), peer.port()]),
                SocketAddr::V6(peer) => json!([
                    peer.ip().to_string(),
                    peer.port(),
                    peer.flowinfo(),
                    peer.scope_id()
                ]),
            };
            let mut evidence = OpenOptions::new()
                .append(true)
                .open(self.root.join("unexpected"))?;
            serde_json::to_writer(
                &mut evidence,
                &json!({"peer":address,"bytes":message.len()}),
            )?;
            evidence.write_all(b"\n")?;
        } else {
            connection.write_all(b"ok\n")?;
        }
        Ok(stopping)
    }

    pub fn serve(self, ready: &mut impl Write) -> Result<(), Error> {
        serde_json::to_writer(
            &mut *ready,
            &json!({"ready": true, "pid": std::process::id()}),
        )?;
        ready.write_all(b"\n")?;
        ready.flush()?;
        loop {
            for listener in &self.listeners {
                match listener.accept() {
                    Ok((connection, peer)) => {
                        if self.connection(connection, peer)? {
                            fs::remove_file(self.root.join("token"))?;
                            fs::remove_file(self.root.join("unexpected"))?;
                            fs::remove_file(&self.outside)?;
                            fs::remove_dir(&self.root)?;
                            return Ok(());
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error.into()),
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn require_service() -> Result<(), Error> {
    if !rustix::process::geteuid().is_root()
        || fs::read("/etc/wrix/nix-pool-service")? != b"1\n"
        || Path::new("/etc/wrix/image-agent").try_exists()?
    {
        return Err(Error::Service);
    }
    Ok(())
}

pub fn run(args: &[OsString]) -> Result<(), Error> {
    let (root, outside, port) = match args {
        [operation] if operation == "boot-id" => {
            require_service()?;
            return control::boot_id(
                Path::new("/proc/sys/kernel/random/boot_id"),
                &mut io::stdout().lock(),
            );
        }
        [operation] if operation == "stop" => {
            require_service()?;
            return control::stop(Path::new("/run/wrix-native-platform-control"), 18443);
        }
        [operation, root, port] if operation == "fixture-stop" => {
            let port = port
                .to_str()
                .and_then(|value| value.parse().ok())
                .ok_or(Error::Arguments)?;
            return control::stop(Path::new(root), port);
        }
        [operation] if operation == "serve" => {
            require_service()?;
            (
                Path::new("/run/wrix-native-platform-control"),
                Path::new("/var/empty/native-platform-outside-build"),
                18443,
            )
        }
        [operation, root, outside, port] if operation == "fixture-serve" => {
            let port = port
                .to_str()
                .and_then(|value| value.parse().ok())
                .ok_or(Error::Arguments)?;
            (Path::new(root), Path::new(outside), port)
        }
        _ => return Err(Error::Arguments),
    };
    Server::bind(root, outside, port)?.serve(&mut io::stdout().lock())
}

#[cfg(test)]
mod test;
