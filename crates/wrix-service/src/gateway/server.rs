//! Authenticated native HTTP/CONNECT role; host routing and launch ownership stay outside this process.

#[path = "bootstrap/mod.rs"]
pub mod bootstrap;
#[path = "management/server.rs"]
pub mod management;
#[path = "traffic/mod.rs"]
mod traffic;
#[path = "transport/mod.rs"]
mod transport;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    net::SocketAddr,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, watch},
    task::JoinSet,
};
pub use wrix_service::gateway::{Error, Secret, policy};

#[derive(Clone)]
struct Grant {
    policy: policy::Policy,
    cancel: watch::Sender<bool>,
    probe: bool,
    active: bool,
}
struct Table {
    grants: HashMap<Secret, Grant>,
    seen: HashSet<Secret>,
    draining: bool,
}
#[derive(Clone)]
struct State {
    table: Arc<Mutex<Table>>,
    connections: Arc<Semaphore>,
    capacity: usize,
    protected: Arc<Vec<SocketAddr>>,
    baseline: policy::Baseline,
    network: Arc<dyn transport::Network>,
}
impl State {
    fn attach(
        &self,
        credential: Secret,
        policy: policy::Policy,
        probe: bool,
        active: bool,
    ) -> Result<Revoker, Error> {
        let mut table = self.table.lock().map_err(|_| Error::Poisoned)?;
        if table.draining || table.seen.contains(&credential) {
            return Err(Error::Denied);
        }
        if table.seen.len() >= 65536 {
            return Err(Error::Capacity);
        }
        let (cancel, _) = watch::channel(false);
        table.seen.insert(credential.clone());
        table.grants.insert(
            credential.clone(),
            Grant {
                policy,
                cancel,
                probe,
                active,
            },
        );
        Ok(Revoker {
            state: self.clone(),
            credential,
        })
    }
    fn grant(&self, credential: &Secret) -> Result<Grant, Error> {
        self.table
            .lock()
            .map_err(|_| Error::Poisoned)?
            .grants
            .get(credential)
            .filter(|grant| grant.active)
            .cloned()
            .ok_or(Error::Denied)
    }
    fn status(&self) -> Result<management::Status, Error> {
        let table = self.table.lock().map_err(|_| Error::Poisoned)?;
        Ok(management::Status {
            version: management::VERSION,
            draining: table.draining,
            connections: self.capacity - self.connections.available_permits(),
            grants: table.grants.len(),
        })
    }
}
struct Revoker {
    state: State,
    credential: Secret,
}
impl Revoker {
    fn activate(&self) -> Result<(), Error> {
        let mut table = self.state.table.lock().map_err(|_| Error::Poisoned)?;
        let grant = table
            .grants
            .get_mut(&self.credential)
            .ok_or(Error::Denied)?;
        if *grant.cancel.borrow() {
            return Err(Error::Denied);
        }
        grant.active = true;
        Ok(())
    }
}
impl Drop for Revoker {
    fn drop(&mut self) {
        match self.state.table.lock() {
            Ok(mut table) => {
                if let Some(grant) = table.grants.remove(&self.credential) {
                    grant.cancel.send_replace(true);
                }
            }
            Err(error) => {
                let mut table = error.into_inner();
                if let Some(grant) = table.grants.remove(&self.credential) {
                    grant.cancel.send_replace(true);
                }
                tracing::error!(role = "egress", "gateway revocation lock poisoned");
            }
        }
    }
}

/// Run in the core service container with distinct data/private-management listeners.
/// Management keys are generation-scoped runtime authority, not host issuer files.
pub async fn run(
    data: TcpListener,
    management: TcpListener,
    key: Secret,
    protected: Vec<SocketAddr>,
    capacity: NonZeroUsize,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), Error> {
    run_network(
        data,
        management,
        key,
        protected,
        capacity,
        Arc::new(transport::System),
        shutdown,
    )
    .await
}

async fn run_network(
    data: TcpListener,
    management: TcpListener,
    key: Secret,
    protected: Vec<SocketAddr>,
    capacity: NonZeroUsize,
    network: Arc<dyn transport::Network>,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), Error> {
    if capacity.get() > 4096 || protected.len() > 4096 {
        return Err(Error::Capacity);
    }
    let state = State {
        table: Arc::new(Mutex::new(Table {
            grants: HashMap::new(),
            seen: HashSet::new(),
            draining: false,
        })),
        connections: Arc::new(Semaphore::new(capacity.get())),
        capacity: capacity.get(),
        protected: Arc::new(protected),
        baseline: policy::Baseline::load()?,
        network,
    };
    let management_capacity = Arc::new(Semaphore::new(1024));
    let mut tasks = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            Some(result) = tasks.join_next() => { if let Err(error) = result { tracing::error!(%error, "gateway connection task failed"); } }
            accepted = data.accept() => {
                let (socket, _) = accepted?;
                let draining = state.table.lock().map_err(|_| Error::Poisoned)?.draining;
                if draining { continue; }
                if let Ok(permit) = state.connections.clone().try_acquire_owned() {
                    let state = state.clone();
                    tasks.spawn(async move {
                        if traffic::serve(socket, state, Arc::new(permit)).await.is_err() { tracing::debug!(role = "egress", "gateway data connection failed"); }
                    });
                }
            }
            accepted = management.accept() => {
                let (socket, _) = accepted?;
                if let Ok(permit) = management_capacity.clone().try_acquire_owned() {
                    let state = state.clone(); let key = key.clone();
                    tasks.spawn(async move {
                        let _permit = permit;
                        if management::serve(socket, &key, &state).await.is_err() { tracing::debug!(role = "egress-management", "gateway private connection failed"); }
                    });
                }
            }
        }
    }
    tasks.abort_all();
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
        {
            return Err(std::io::Error::other(error).into());
        }
    }
    Ok(())
}

async fn cancelled(mut receiver: watch::Receiver<bool>) {
    loop {
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

/// Native role entry without host issuer files, worker policies or durable grants.
pub async fn serve_gateway() -> crate::lifecycle::Result<()> {
    use std::net::Ipv4Addr;
    let key = crate::lifecycle::generation::gateway_secret()?;
    let data = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 3128)).await?;
    let management = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 3129)).await?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    run(
        data,
        management,
        key,
        vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 3129))],
        NonZeroUsize::MIN.saturating_add(1023),
        async move {
            terminate.recv().await;
        },
    )
    .await
    .map_err(|error| std::io::Error::other(error).into())
}

#[cfg(test)]
#[path = "test/mod.rs"]
pub(crate) mod test;
