//! Generation-scoped bootstrap data delivered over a private child stdin, never argv or a guest mount.

use super::{Error, Secret};
use serde::Deserialize;
use std::{net::SocketAddr, num::NonZeroUsize};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    net::TcpListener,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub version: u32,
    pub data: SocketAddr,
    pub management: SocketAddr,
    pub key: Secret,
    pub protected: Vec<SocketAddr>,
    pub capacity: NonZeroUsize,
}
impl Launch {
    pub fn client(&self) -> Result<super::management::Client, Error> {
        self.check()?;
        Ok(super::management::Client::new(
            self.management,
            self.key.clone(),
        ))
    }
    fn check(&self) -> Result<(), Error> {
        if self.version != super::management::VERSION
            || self.data == self.management
            || self.data.port() == 0
            || self.management.port() == 0
            || self.capacity.get() > 4096
            || self.protected.len() > 4096
            || !self.protected.contains(&self.management)
            || !self.protected.contains(&self.data)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub async fn run(self) -> Result<(), Error> {
        self.check()?;
        let data = TcpListener::bind(self.data).await?;
        let management = TcpListener::bind(self.management).await?;
        super::run(
            data,
            management,
            self.key,
            self.protected,
            self.capacity,
            std::future::pending(),
        )
        .await
    }
}

pub async fn from_stdin(input: impl AsyncRead + Unpin) -> Result<(), Error> {
    read(input).await?.run().await
}

pub async fn read(input: impl AsyncRead + Unpin) -> Result<Launch, Error> {
    let mut data = Vec::new();
    input.take(65537).read_to_end(&mut data).await?;
    if data.len() > 65536 {
        return Err(Error::Invalid);
    }
    let launch: Launch = serde_json::from_slice(&data)?;
    launch.check()?;
    Ok(launch)
}
