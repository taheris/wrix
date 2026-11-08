//! Host-owned Rama forwarding engines over fixed-role listener capabilities.

use rama_core::{Service, io::BridgeIo, rt::Executor};
use rama_net::proxy::IoForwardService;
use rama_tcp::server::TcpListener;
use std::{
    future::Future,
    io,
    net::SocketAddr,
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpStream, task::JoinSet};
pub use wrix_service::route::{Error, Port, Role};

#[cfg(test)]
#[path = "guest_test.rs"]
mod guest_test;

const BUFFER_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Listeners([TcpListener; 5]);
impl Listeners {
    pub(crate) fn from_core(listeners: wrix_service::route::Listeners) -> Self {
        Self::from_native(listeners.into_native())
    }
    pub(crate) fn from_native(listeners: [tokio::net::TcpListener; 5]) -> Self {
        Self(
            listeners.map(|listener| {
                TcpListener::from_tokio_tcp_listener(listener, Executor::default())
            }),
        )
    }
    pub async fn bind(ports: [Port; 4]) -> Result<Self, Error> {
        Ok(Self::from_core(
            wrix_service::route::Listeners::bind(ports).await?,
        ))
    }
    pub async fn bind_with_sync(ports: [Port; 5]) -> Result<Self, Error> {
        Ok(Self::from_core(
            wrix_service::route::Listeners::bind_with_sync(ports).await?,
        ))
    }
    pub fn address(&self, role: Role) -> Result<SocketAddr, Error> {
        Ok(self.0[role.index()].local_addr()?)
    }
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
    /// Call only after native authenticated readiness, not merely a successful connection.
    pub fn confirm_ready(&self, role: Role) -> ReadyRoute {
        ReadyRoute {
            role,
            target: self.0[role.index()].clone(),
        }
    }
    pub fn connections(&self, role: Role) -> usize {
        self.0[role.index()].connections.load(Ordering::Acquire)
    }
}

/// Not deserializable or constructible from guest addresses or container reports.
///
/// ```compile_fail
/// use wrix_service_server::route::{Selection, Role};
/// let selection = Selection::default();
/// selection.select("127.0.0.1:1234".parse::<std::net::SocketAddr>().unwrap());
/// ```
///
/// ```compile_fail
/// use wrix_service_server::route::Role;
/// let role = Role::Management;
/// ```
///
/// ```compile_fail
/// use wrix_service_server::route::Role;
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
