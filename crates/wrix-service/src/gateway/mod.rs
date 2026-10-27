//! Authenticated native HTTP/CONNECT role; host routing and launch ownership stay outside this process.

pub mod bootstrap;
pub mod management;
pub mod policy;
mod traffic;
mod transport;

use serde::{Deserialize, Serialize};
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

/// Gateway operation failed: no authorization is implied by an error.
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// gateway I/O failed: {0}
    Io(#[from] std::io::Error),
    /// invalid gateway protocol or policy
    Invalid,
    /// gateway authorization denied
    Denied,
    /// gateway destination is protected or outside policy
    Protected,
    /// gateway capacity exhausted
    Capacity,
    /// gateway state lock poisoned
    Poisoned,
    /// gateway protocol encoding failed: {0}
    Json(#[from] serde_json::Error),
}

/// An unguessable scoped secret; Debug never emits its contents.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Secret(String);
impl Secret {
    pub fn generate() -> Result<Self, Error> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
        let mut encoded = String::with_capacity(64);
        for byte in bytes {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 15)]));
        }
        Ok(Self(encoded))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn matches(&self, other: &Self) -> bool {
        self.0
            .bytes()
            .zip(other.0.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl TryFrom<String> for Secret {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid);
        }
        Ok(Self(value))
    }
}
impl From<Secret> for String {
    fn from(value: Secret) -> Self {
        value.0
    }
}

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

#[cfg(test)]
pub(crate) mod test;
