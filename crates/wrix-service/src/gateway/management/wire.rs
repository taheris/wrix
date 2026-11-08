//! Bounded versioned management frames shared by the client and server engines.

use crate::gateway::{Error, Secret, policy::Policy};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub const VERSION: u32 = 1;
const MAX_FRAME: usize = 65536;
pub const BUDGET: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: u32,
    pub key: Secret,
    pub operation: Operation,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum Operation {
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
pub enum Reply {
    Status(Status),
    Attached,
    Activated,
    Detached,
    Denied,
}

pub async fn read<T: DeserializeOwned>(stream: &mut TcpStream) -> Result<T, Error> {
    let size = stream.read_u32().await? as usize;
    if size > MAX_FRAME {
        return Err(Error::Invalid);
    }
    let mut data = vec![0; size];
    stream.read_exact(&mut data).await?;
    Ok(serde_json::from_slice(&data)?)
}
pub async fn write<T: Serialize + Sync>(stream: &mut TcpStream, value: &T) -> Result<(), Error> {
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
