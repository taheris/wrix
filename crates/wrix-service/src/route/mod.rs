//! Host-owned, byte-transparent fixed-role transport; not a guest control interface.

use std::{
    future::Future,
    io,
    net::{Ipv4Addr, SocketAddr},
    num::{NonZeroU16, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use displaydoc::Display;
use rama_core::{Service, io::BridgeIo, rt::Executor};
use rama_net::proxy::IoForwardService;
use rama_tcp::server::TcpListener;
use thiserror::Error as ThisError;
use tokio::{net::TcpStream, task::JoinSet};

#[cfg(test)]
mod guest_test;

const BUFFER_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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

    const fn index(self) -> usize {
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
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn from_native(listeners: [tokio::net::TcpListener; 5]) -> Self {
        Self(
            listeners.map(|listener| {
                TcpListener::from_tokio_tcp_listener(listener, Executor::default())
            }),
        )
    }

    /// Bind the complete data listener set on physical-host loopback before publication.
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

    /// Split generation listeners from their host-only route capabilities before serving.
    pub fn into_generation(self) -> Result<([TcpListener; 5], Generation), Error> {
        let target = |role| -> Result<Arc<Target>, Error> {
            Ok(Arc::new(Target {
                address: self.address(role)?,
                connections: AtomicUsize::new(0),
            }))
        };
        let generation = Generation([
            target(Role::Sql)?,
            target(Role::CacheRead)?,
            target(Role::Egress)?,
            target(Role::Compiler)?,
            target(Role::Sync)?,
        ]);
        Ok((self.0, generation))
    }
}

async fn bind(port: Port) -> Result<TcpListener, Error> {
    let port = match port {
        Port::Available => 0,
        Port::Fixed(port) => port.get(),
    };
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    Ok(TcpListener::from_tokio_tcp_listener(
        listener,
        Executor::default(),
    ))
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

struct Target {
    address: SocketAddr,
    connections: AtomicUsize,
}

/// Host-owned capabilities refer only to the generation's creation-time data listeners.
#[derive(Clone)]
pub struct Generation([Arc<Target>; 5]);

impl Generation {
    pub(crate) fn from_native(addresses: [SocketAddr; 5]) -> Result<Self, Error> {
        if addresses
            .iter()
            .any(|address| !address.ip().is_loopback() || address.port() == 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native roles require fixed host-loopback addresses",
            )
            .into());
        }
        Ok(Self(addresses.map(|address| {
            Arc::new(Target {
                address,
                connections: AtomicUsize::new(0),
            })
        })))
    }

    pub(crate) fn reuse_role(&mut self, previous: &Self, role: Role) {
        self.0[role.index()] = previous.0[role.index()].clone();
    }

    /// Call only after the controller establishes this role's native authenticated readiness.
    /// A successful TCP connection alone is not readiness or backend ownership evidence.
    pub fn confirm_ready(&self, role: Role) -> ReadyRoute {
        ReadyRoute {
            role,
            target: self.0[role.index()].clone(),
        }
    }

    /// Transport accounting includes pending connects, not native backend work completion.
    pub fn connections(&self, role: Role) -> usize {
        self.0[role.index()].connections.load(Ordering::Acquire)
    }
}

/// Not deserializable or constructible from guest-supplied addresses or container reports.
///
/// ```compile_fail
/// use wrix_service::route::{Selection, Role};
/// let selection = Selection::default();
/// selection.select("127.0.0.1:1234".parse::<std::net::SocketAddr>().unwrap());
/// ```
///
/// ```compile_fail
/// use wrix_service::route::Role;
/// let role = Role::Management;
/// ```
///
/// ```compile_fail
/// use wrix_service::route::Role;
/// let role = Role::Publication;
/// ```
pub struct ReadyRoute {
    role: Role,
    target: Arc<Target>,
}

#[derive(Clone, Default)]
pub struct Selection(Arc<Mutex<[Option<Arc<Target>>; 5]>>);

impl Selection {
    /// Switch only future admissions; accepted connections retain their original target.
    pub fn select(&self, ready: ReadyRoute) -> Result<(), Error> {
        self.0.lock().map_err(|_| Error::LockPoisoned)?[ready.role.index()] = Some(ready.target);
        Ok(())
    }

    pub fn close(&self, role: Role) -> Result<(), Error> {
        self.0.lock().map_err(|_| Error::LockPoisoned)?[role.index()] = None;
        Ok(())
    }

    fn admit(&self, role: Role) -> Result<Option<Connection>, Error> {
        let selected = self.0.lock().map_err(|_| Error::LockPoisoned)?;
        Ok(selected[role.index()].as_ref().map(|target| {
            target.connections.fetch_add(1, Ordering::AcqRel);
            Connection(target.clone())
        }))
    }
}

struct Connection(Arc<Target>);

impl Drop for Connection {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One bounded front door for the deployment, independent of workspace count.
pub struct FrontDoor {
    listeners: Listeners,
    guest: Option<(Listeners, crate::control::serving::apple::Address)>,
    selection: Selection,
    capacity: usize,
}

impl FrontDoor {
    pub fn new(listeners: Listeners, capacity: NonZeroUsize) -> Result<Self, Error> {
        if capacity.get() > MAX_CONNECTIONS {
            return Err(Error::CapacityExceeded);
        }
        Ok(Self {
            listeners,
            guest: None,
            selection: Selection::default(),
            capacity: capacity.get(),
        })
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn with_guest(
        mut self,
        listeners: Listeners,
        address: crate::control::serving::apple::Address,
    ) -> Self {
        self.guest = Some((listeners, address));
        self
    }

    async fn accept_guest(
        &self,
        role: Role,
    ) -> io::Result<(rama_tcp::TcpStream, rama_net::address::SocketAddress)> {
        match &self.guest {
            Some((listeners, _)) => listeners.0[role.index()].accept().await,
            None => std::future::pending().await,
        }
    }

    pub fn selection(&self) -> Selection {
        self.selection.clone()
    }

    /// Bound loopback publication, not evidence of selected native backend readiness.
    pub fn address(&self, role: Role) -> Result<SocketAddr, Error> {
        self.listeners.address(role)
    }

    /// Stop accepting on shutdown, then await existing streams without an idle deadline.
    pub async fn serve(self, shutdown: impl Future<Output = ()> + Send) -> Result<(), Error> {
        let [sql, cache, egress, compiler, sync] = &self.listeners.0;
        let mut tasks = JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            let (role, accepted, guest) = tokio::select! {
                () = &mut shutdown => break,
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(result) = result { result?; }
                    continue;
                }
                stream = sql.accept() => (Role::Sql, stream, false),
                stream = cache.accept() => (Role::CacheRead, stream, false),
                stream = egress.accept() => (Role::Egress, stream, false),
                stream = compiler.accept() => (Role::Compiler, stream, false),
                stream = sync.accept() => (Role::Sync, stream, false),
                stream = self.accept_guest(Role::Sql) => (Role::Sql, stream, true),
                stream = self.accept_guest(Role::CacheRead) => (Role::CacheRead, stream, true),
                stream = self.accept_guest(Role::Egress) => (Role::Egress, stream, true),
                stream = self.accept_guest(Role::Compiler) => (Role::Compiler, stream, true),
                stream = self.accept_guest(Role::Sync) => (Role::Sync, stream, true),
            };
            let (stream, peer) = accepted?;
            let peer: SocketAddr = peer.into();
            if guest && !self.guest.as_ref().is_some_and(|(_, address)| {
                matches!(peer.ip(), std::net::IpAddr::V4(ip) if address.accepts(ip))
            }) {
                tracing::warn!(?role, %peer, "guest-only role listener refused an outside-subnet peer");
                continue;
            }
            while let Some(result) = tasks.try_join_next() {
                result?;
            }
            if tasks.len() >= self.capacity {
                tracing::warn!(?role, "fixed-role connection capacity exhausted");
                continue;
            }
            let Some(connection) = self.selection.admit(role)? else {
                tracing::debug!(?role, "fixed-role admission closed");
                continue;
            };
            tasks.spawn(async move {
                let _connection = &connection;
                match tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    TcpStream::connect(connection.0.address),
                )
                .await
                {
                    Ok(Ok(upstream)) => {
                        let forward =
                            IoForwardService::new(Executor::default()).with_buf_size(BUFFER_BYTES);
                        if let Err(error) = forward.serve(BridgeIo(stream, upstream)).await {
                            tracing::warn!(?role, %error, "fixed-role stream failed");
                        }
                    }
                    Ok(Err(error)) => tracing::warn!(?role, %error, "fixed-role connect failed"),
                    Err(error) => tracing::warn!(?role, %error, "fixed-role connect timed out"),
                }
            });
        }
        drop(self.listeners);
        drop(self.guest);
        while let Some(result) = tasks.join_next().await {
            result?;
        }
        Ok(())
    }
}
