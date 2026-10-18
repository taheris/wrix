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

const BUFFER_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// fixed-role listener I/O failed: {0}
    Io(#[from] io::Error),
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
}

impl Role {
    pub const ALL: [Self; 4] = [Self::Sql, Self::CacheRead, Self::Egress, Self::Compiler];

    const fn index(self) -> usize {
        match self {
            Self::Sql => 0,
            Self::CacheRead => 1,
            Self::Egress => 2,
            Self::Compiler => 3,
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
pub struct Listeners([TcpListener; 4]);

impl Listeners {
    /// Bind the complete data listener set on physical-host loopback before publication.
    pub async fn bind(ports: [Port; 4]) -> Result<Self, Error> {
        let [sql, cache, egress, compiler] = ports;
        Ok(Self([
            bind(sql).await?,
            bind(cache).await?,
            bind(egress).await?,
            bind(compiler).await?,
        ]))
    }

    pub fn address(&self, role: Role) -> Result<SocketAddr, Error> {
        Ok(self.0[role.index()].local_addr()?)
    }

    /// Split generation listeners from their host-only route capabilities before serving.
    pub fn into_generation(self) -> Result<([TcpListener; 4], Generation), Error> {
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

struct Target {
    address: SocketAddr,
    connections: AtomicUsize,
}

/// Host-owned capabilities refer only to the generation's creation-time data listeners.
#[derive(Clone)]
pub struct Generation([Arc<Target>; 4]);

impl Generation {
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
pub struct Selection(Arc<Mutex<[Option<Arc<Target>>; 4]>>);

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
            selection: Selection::default(),
            capacity: capacity.get(),
        })
    }

    pub fn selection(&self) -> Selection {
        self.selection.clone()
    }

    /// Stop accepting on shutdown, then await existing streams without an idle deadline.
    pub async fn serve(self, shutdown: impl Future<Output = ()> + Send) -> Result<(), Error> {
        let [sql, cache, egress, compiler] = &self.listeners.0;
        let mut tasks = JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            let (role, accepted) = tokio::select! {
                () = &mut shutdown => break,
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(result) = result { result?; }
                    continue;
                }
                stream = sql.accept() => (Role::Sql, stream),
                stream = cache.accept() => (Role::CacheRead, stream),
                stream = egress.accept() => (Role::Egress, stream),
                stream = compiler.accept() => (Role::Compiler, stream),
            };
            let (stream, _) = accepted?;
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
        while let Some(result) = tasks.join_next().await {
            result?;
        }
        Ok(())
    }
}
