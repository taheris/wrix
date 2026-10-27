//! Fixed authenticated private protocol; never served by the traffic listener.

use super::{Error, Secret, State, policy::Policy};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

pub const VERSION: u32 = 1;
const MAX_FRAME: usize = 65536;
const BUDGET: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    key: Secret,
    operation: Operation,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
enum Operation {
    Status,
    Drain { closed: bool },
    Attach { credential: Secret, policy: Policy },
    Prepare { credential: Secret, policy: Policy },
    Probe { credential: Secret },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub version: u32,
    pub draining: bool,
    pub connections: usize,
    pub grants: usize,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "result", deny_unknown_fields)]
enum Reply {
    Status(Status),
    Attached,
    Activated,
    Detached,
    Denied,
}

async fn read<T: DeserializeOwned>(stream: &mut TcpStream) -> Result<T, Error> {
    let size = stream.read_u32().await? as usize;
    if size > MAX_FRAME {
        return Err(Error::Invalid);
    }
    let mut data = vec![0; size];
    stream.read_exact(&mut data).await?;
    Ok(serde_json::from_slice(&data)?)
}
async fn write<T: Serialize + Sync>(stream: &mut TcpStream, value: &T) -> Result<(), Error> {
    let data = serde_json::to_vec(value)?;
    if data.len() > MAX_FRAME {
        return Err(Error::Invalid);
    }
    stream
        .write_u32(u32::try_from(data.len()).map_err(|_| Error::Invalid)?)
        .await?;
    stream.write_all(&data).await?;
    Ok(())
}

pub(super) async fn serve(mut stream: TcpStream, key: &Secret, state: &State) -> Result<(), Error> {
    let envelope: Envelope = timeout(BUDGET, read(&mut stream))
        .await
        .map_err(std::io::Error::other)??;
    if envelope.version != VERSION || !key.matches(&envelope.key) {
        write(&mut stream, &Reply::Denied).await?;
        return Err(Error::Denied);
    }
    match envelope.operation {
        Operation::Status => write(&mut stream, &Reply::Status(state.status()?)).await,
        Operation::Drain { closed } => {
            state.table.lock().map_err(|_| Error::Poisoned)?.draining = closed;
            write(&mut stream, &Reply::Status(state.status()?)).await
        }
        Operation::Attach { credential, policy } => {
            hold(&mut stream, state.attach(credential, policy, false, true)?).await
        }
        Operation::Prepare { credential, policy } => {
            hold(&mut stream, state.attach(credential, policy, false, false)?).await
        }
        Operation::Probe { credential } => {
            hold(
                &mut stream,
                state.attach(credential, probe_policy()?, true, true)?,
            )
            .await
        }
    }
}

fn probe_policy() -> Result<Policy, Error> {
    Policy::new(vec![super::policy::Rule::new(
        super::policy::Host::parse("probe.wrix.invalid")?,
        vec![std::num::NonZeroU16::new(80).ok_or(Error::Invalid)?],
    )?])
}

async fn hold(stream: &mut TcpStream, revoker: super::Revoker) -> Result<(), Error> {
    write(stream, &Reply::Attached).await?;
    let mut byte = [0];
    loop {
        match stream.read(&mut byte).await? {
            0 => return Ok(()),
            1 if byte[0] == 0 => {
                drop(revoker);
                return write(stream, &Reply::Detached).await;
            }
            1 if byte[0] == 1 => {
                revoker.activate()?;
                write(stream, &Reply::Activated).await?;
            }
            _ => return Err(Error::Invalid),
        }
    }
}

/// The generation manager fixes this endpoint and key; workers cannot choose either.
#[derive(Clone, Debug)]
pub struct Client {
    address: SocketAddr,
    key: Secret,
}
impl Client {
    pub const fn new(address: SocketAddr, key: Secret) -> Self {
        Self { address, key }
    }
    async fn request(&self, operation: Operation) -> Result<(TcpStream, Reply), Error> {
        timeout(BUDGET, async {
            let mut stream = TcpStream::connect(self.address).await?;
            write(
                &mut stream,
                &Envelope {
                    version: VERSION,
                    key: self.key.clone(),
                    operation,
                },
            )
            .await?;
            let reply = read(&mut stream).await?;
            Ok((stream, reply))
        })
        .await
        .map_err(std::io::Error::other)?
    }
    pub async fn status(&self) -> Result<Status, Error> {
        match self.request(Operation::Status).await?.1 {
            Reply::Status(status) if status.version == VERSION => Ok(status),
            _ => Err(Error::Denied),
        }
    }
    pub fn same_generation(&self, other: &Self) -> bool {
        self.address == other.address && self.key.matches(&other.key)
    }
    /// Verify compatible native management and permitted/denied authenticated traffic admission.
    /// This does not claim external DNS/upstream availability.
    pub async fn verify(&self, data: SocketAddr) -> Result<Status, Error> {
        timeout(BUDGET, self.verify_inner(data))
            .await
            .map_err(std::io::Error::other)?
    }
    async fn verify_inner(&self, data: SocketAddr) -> Result<Status, Error> {
        let status = self.status().await?;
        if status.draining {
            return Err(Error::Denied);
        }
        let credential = Secret::generate()?;
        let (attachment, reply) = self
            .request(Operation::Probe {
                credential: credential.clone(),
            })
            .await?;
        if !matches!(reply, Reply::Attached) {
            return Err(Error::Denied);
        }
        for (host, authenticated, expected) in [
            ("probe.wrix.invalid", true, 204),
            ("denied.wrix.invalid", true, 403),
            ("probe.wrix.invalid", false, 407),
        ] {
            let mut socket = TcpStream::connect(data).await?;
            let auth = if authenticated {
                format!("Proxy-Authorization: Bearer {}\r\n", credential.expose())
            } else {
                String::new()
            };
            let request = format!(
                "GET http://{host}/ HTTP/1.1\r\nHost: {host}\r\n{auth}Connection: close\r\n\r\n"
            );
            socket.write_all(request.as_bytes()).await?;
            let mut response = Vec::new();
            loop {
                if response.len() >= 4096 {
                    return Err(Error::Invalid);
                }
                let byte = socket.read_u8().await?;
                response.push(byte);
                if byte == b'\n' {
                    break;
                }
            }
            let response = std::str::from_utf8(&response).map_err(|_| Error::Invalid)?;
            if !response.starts_with(&format!("HTTP/1.1 {expected} ")) {
                return Err(Error::Denied);
            }
        }
        drop(attachment);
        Ok(status)
    }
    /// Close only new TCP admission, never old keep-alive requests or active streams.
    pub async fn drain(&self, closed: bool) -> Result<Status, Error> {
        match self.request(Operation::Drain { closed }).await?.1 {
            Reply::Status(status) if status.version == VERSION => Ok(status),
            _ => Err(Error::Denied),
        }
    }
    /// Preparation installs no traffic authority until that private attachment is activated.
    pub async fn prepare(&self, credential: Secret, policy: Policy) -> Result<Attachment, Error> {
        let (stream, reply) = self
            .request(Operation::Prepare { credential, policy })
            .await?;
        match reply {
            Reply::Attached => Ok(Attachment(stream)),
            _ => Err(Error::Denied),
        }
    }
    /// Hold the returned channel until the live launcher revokes, including after generation transfer.
    pub async fn attach(&self, credential: Secret, policy: Policy) -> Result<Attachment, Error> {
        let (stream, reply) = self
            .request(Operation::Attach { credential, policy })
            .await?;
        match reply {
            Reply::Attached => Ok(Attachment(stream)),
            _ => Err(Error::Denied),
        }
    }
}

/// Closing this channel revokes the grant in that generation; it is not worker exit evidence.
pub struct Attachment(TcpStream);
impl Attachment {
    pub async fn activate(&mut self) -> Result<(), Error> {
        timeout(BUDGET, async {
            self.0.write_u8(1).await?;
            match read(&mut self.0).await? {
                Reply::Activated => Ok(()),
                _ => Err(Error::Denied),
            }
        })
        .await
        .map_err(std::io::Error::other)?
    }
    pub async fn release(mut self) -> Result<(), Error> {
        timeout(BUDGET, async {
            self.0.write_u8(0).await?;
            match read(&mut self.0).await? {
                Reply::Detached => Ok(()),
                _ => Err(Error::Denied),
            }
        })
        .await
        .map_err(std::io::Error::other)?
    }
    pub async fn lost(&mut self) -> Result<(), Error> {
        let mut byte = [0];
        match self.0.read(&mut byte).await? {
            0 => Ok(()),
            _ => Err(Error::Invalid),
        }
    }
}
