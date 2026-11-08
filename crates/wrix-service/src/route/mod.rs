//! Fixed-role publication and listener capabilities shared by lifecycle and server engines.

use displaydoc::Display;
use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    num::NonZeroU16,
};
use thiserror::Error as ThisError;
use tokio::net::TcpListener;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// fixed-role listener I/O failed: {0}
    Io(#[from] io::Error),
    /// no loopback port is available for deployment role {role:?}
    NoAvailablePort { role: Role },
    /// cannot bind retained endpoint 127.0.0.1:{port} for {role:?}; the endpoint will not be relocated: {source}
    RetainedPort {
        role: Role,
        port: NonZeroU16,
        source: io::Error,
    },
    /// fixed-role listener construction is incomplete
    IncompleteListeners,
    /// fixed-role selection lock is poisoned
    LockPoisoned,
    /// connection capacity exceeds the fixed-role service limit
    CapacityExceeded,
    /// fixed-role forwarding task failed: {0}
    Task(#[from] tokio::task::JoinError),
}

/// Only tenant-facing data roles may be forwarded; control and publishing are absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Sql,
    CacheRead,
    Egress,
    Compiler,
    Sync,
}
impl Role {
    pub const ALL: [Self; 5] = [
        Self::Sql,
        Self::CacheRead,
        Self::Egress,
        Self::Compiler,
        Self::Sync,
    ];
    pub const fn index(self) -> usize {
        match self {
            Self::Sql => 0,
            Self::CacheRead => 1,
            Self::Egress => 2,
            Self::Compiler => 3,
            Self::Sync => 4,
        }
    }
}

/// A creation-time bind request, not an optional or sentinel-valued published endpoint.
#[derive(Clone, Copy, Debug)]
pub enum Port {
    Available,
    Fixed(NonZeroU16),
}

/// A complete listener set allocated at creation, never extended by workspace registration.
pub struct Listeners([TcpListener; 5]);
impl Listeners {
    pub fn into_native(self) -> [TcpListener; 5] {
        self.0
    }

    pub async fn bind(ports: [Port; 4]) -> Result<Self, Error> {
        let [sql, cache, egress, compiler] = ports;
        Self::bind_with_sync([sql, cache, egress, compiler, Port::Available]).await
    }
    pub async fn bind_with_sync(ports: [Port; 5]) -> Result<Self, Error> {
        let [sql, cache, egress, compiler, sync] = ports;
        Ok(Self([
            bind(sql).await?,
            bind(cache).await?,
            bind(egress).await?,
            bind(compiler).await?,
            bind(sync).await?,
        ]))
    }
    pub(crate) async fn bind_deployment(
        prior: [Option<NonZeroU16>; 5],
        retained: [bool; 5],
    ) -> Result<Self, Error> {
        let mut listeners: [Option<TcpListener>; 5] = std::array::from_fn(|_| None);
        for role in Role::ALL {
            if retained[role.index()]
                && let Some(port) = prior[role.index()]
            {
                listeners[role.index()] = Some(bind_retained(role, port).await?);
            }
        }
        for role in Role::ALL {
            if listeners[role.index()].is_none() {
                listeners[role.index()] = Some(bind_role(role, prior[role.index()]).await?);
            }
        }
        let [sql, cache, egress, compiler, sync] = listeners;
        Ok(Self([
            sql.ok_or(Error::IncompleteListeners)?,
            cache.ok_or(Error::IncompleteListeners)?,
            egress.ok_or(Error::IncompleteListeners)?,
            compiler.ok_or(Error::IncompleteListeners)?,
            sync.ok_or(Error::IncompleteListeners)?,
        ]))
    }
    pub fn address(&self, role: Role) -> Result<SocketAddr, Error> {
        Ok(self.0[role.index()].local_addr()?)
    }
}

async fn bind(port: Port) -> Result<TcpListener, Error> {
    let port = match port {
        Port::Available => 0,
        Port::Fixed(port) => port.get(),
    };
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?)
}
async fn bind_retained(role: Role, port: NonZeroU16) -> Result<TcpListener, Error> {
    bind(Port::Fixed(port)).await.map_err(|error| match error {
        Error::Io(source) => Error::RetainedPort { role, port, source },
        other => other,
    })
}
async fn bind_role(role: Role, prior: Option<NonZeroU16>) -> Result<TcpListener, Error> {
    if let Some(port) = prior {
        match bind(Port::Fixed(port)).await {
            Ok(listener) => return Ok(listener),
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::AddrInUse => (),
            Err(error) => return Err(error),
        }
    }
    let range = match role {
        Role::Sql | Role::Sync => 23_000..25_000,
        Role::CacheRead => 21_000..23_000,
        Role::Egress | Role::Compiler => return bind(Port::Available).await,
    };
    for port in range.filter_map(NonZeroU16::new) {
        if Some(port) == prior {
            continue;
        }
        match bind(Port::Fixed(port)).await {
            Ok(listener) => return Ok(listener),
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::AddrInUse => (),
            Err(error) => return Err(error),
        }
    }
    Err(Error::NoAvailablePort { role })
}
