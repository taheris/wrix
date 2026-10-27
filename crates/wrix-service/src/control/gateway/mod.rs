//! Host-only, ephemeral launch issuance and live generation handoff. Never persist grants.

use super::{Authorization, Namespace, Permission};
use crate::gateway::{
    Error, Secret,
    management::{Attachment, Client},
    policy::Policy,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::{Mutex as AsyncMutex, watch},
};

struct Live {
    credential: Secret,
    policy: Policy,
    namespace: Namespace,
    state: Mutex<Ownership>,
    cancel: watch::Sender<bool>,
    links: Mutex<Vec<Link>>,
}
struct Link {
    client: Client,
    detach: watch::Sender<bool>,
    finished: watch::Receiver<bool>,
}
struct Ownership {
    epoch: u64,
    revoked: bool,
}
impl Live {
    fn revoke(&self) {
        match self.state.lock() {
            Ok(mut state) => {
                state.revoked = true;
                self.cancel.send_replace(true);
            }
            Err(error) => {
                self.cancel.send_replace(true);
                tracing::error!(%error, "host gateway ownership lock poisoned");
            }
        }
    }
    fn check(&self, epoch: u64) -> Result<(), Error> {
        let state = self.state.lock().map_err(|_| Error::Poisoned)?;
        if state.revoked || state.epoch != epoch {
            return Err(Error::Denied);
        }
        Ok(())
    }
}
struct Selection {
    selected: Client,
    live: HashMap<Secret, Weak<Live>>,
}

/// One registry belongs to the stable controller, not a replaceable generation or update CLI.
pub struct Issuer(AsyncMutex<Selection>);
impl Issuer {
    pub fn new(selected: Client) -> Self {
        Self(AsyncMutex::new(Selection {
            selected,
            live: HashMap::new(),
        }))
    }

    /// Call only after checked workspace admission and immutable effective policy selection.
    pub async fn issue(
        &self,
        authorization: &Authorization<'_>,
        policy: Policy,
    ) -> Result<Owner, Error> {
        if authorization.permission() != Permission::Egress {
            return Err(Error::Denied);
        }
        self.issue_checked(authorization.namespace().clone(), policy)
            .await
    }
    async fn issue_checked(&self, namespace: Namespace, policy: Policy) -> Result<Owner, Error> {
        let mut selection = self.0.lock().await;
        selection.live.retain(|_, live| live.strong_count() != 0);
        if selection.live.len() >= 4096 {
            return Err(Error::Capacity);
        }
        let credential = Secret::generate()?;
        let attachment = selection
            .selected
            .attach(credential.clone(), policy.clone())
            .await?;
        let (cancel, _) = watch::channel(false);
        let live = Arc::new(Live {
            credential: credential.clone(),
            policy,
            namespace,
            state: Mutex::new(Ownership {
                epoch: 0,
                revoked: false,
            }),
            cancel,
            links: Mutex::new(Vec::new()),
        });
        follow(attachment, &live, selection.selected.clone())?;
        selection.live.insert(credential, Arc::downgrade(&live));
        Ok(Owner { live, epoch: 0 })
    }

    /// Serialize issuance with preparation. All live policies must be installed before selecting Green.
    /// The caller switches stable HOST new-connection routing only after this succeeds.
    pub async fn handoff(&self, green: Client) -> Result<(), Error> {
        if green.status().await?.draining {
            return Err(Error::Denied);
        }
        let mut selection = self.0.lock().await;
        if selection.selected.same_generation(&green) {
            return Ok(());
        }
        if selection.selected.status().await.is_err() {
            for weak in selection.live.values() {
                if let Some(live) = weak.upgrade() {
                    live.revoke();
                }
            }
            return Err(Error::Denied);
        }
        let mut prepared = Vec::new();
        for weak in selection.live.values() {
            if let Some(live) = weak.upgrade() {
                let active = !live.state.lock().map_err(|_| Error::Poisoned)?.revoked;
                if active {
                    if live.links.lock().map_err(|_| Error::Poisoned)?.len() >= 32 {
                        return Err(Error::Capacity);
                    }
                    let attachment = green
                        .prepare(live.credential.clone(), live.policy.clone())
                        .await?;
                    prepared.push((attachment, live));
                }
            }
        }
        for (attachment, live) in &mut prepared {
            let mut cancel = live.cancel.subscribe();
            tokio::select! {
                biased;
                () = async { loop { if *cancel.borrow_and_update() || cancel.changed().await.is_err() { break; } } } => (),
                result = attachment.activate() => result?,
            }
        }
        for (attachment, live) in prepared {
            if live.state.lock().map_err(|_| Error::Poisoned)?.revoked {
                drop(attachment);
            } else {
                follow(attachment, &live, green.clone())?;
            }
        }
        selection.selected = green;
        Ok(())
    }
    /// Only after Blue closes new admission and its protected data connections have drained.
    /// Detach transferred grants before expected Blue shutdown, without revoking Green ownership.
    pub async fn release_drained(&self, blue: &Client) -> Result<(), Error> {
        let selection = self.0.lock().await;
        if selection.selected.same_generation(blue) {
            return Err(Error::Denied);
        }
        let status = blue.status().await?;
        if !status.draining || status.connections != 0 {
            return Err(Error::Denied);
        }
        let mut pending = Vec::new();
        for weak in selection.live.values() {
            if let Some(live) = weak.upgrade() {
                let mut links = live.links.lock().map_err(|_| Error::Poisoned)?;
                links.retain(|link| {
                    if link.client.same_generation(blue) {
                        link.detach.send_replace(true);
                        pending.push(link.finished.clone());
                        false
                    } else {
                        true
                    }
                });
            }
        }
        for mut finished in pending {
            while !*finished.borrow_and_update() {
                finished.changed().await.map_err(|_| Error::Denied)?;
            }
        }
        if blue.status().await?.grants != 0 {
            return Err(Error::Denied);
        }
        Ok(())
    }
    pub async fn revoke_namespace(&self, namespace: &Namespace) {
        for weak in self.0.lock().await.live.values() {
            if let Some(live) = weak.upgrade()
                && live.namespace == *namespace
            {
                live.revoke();
            }
        }
    }
    pub async fn revoke_all(&self) {
        for weak in self.0.lock().await.live.values() {
            if let Some(live) = weak.upgrade() {
                live.revoke();
            }
        }
    }
}

fn follow(mut attachment: Attachment, live: &Arc<Live>, client: Client) -> Result<(), Error> {
    let mut links = live.links.lock().map_err(|_| Error::Poisoned)?;
    if links.len() >= 32 {
        return Err(Error::Capacity);
    }
    let (detach, mut detached) = watch::channel(false);
    let (done, finished) = watch::channel(false);
    links.push(Link {
        client,
        detach,
        finished,
    });
    let mut cancel = live.cancel.subscribe();
    let weak = Arc::downgrade(live);
    tokio::spawn(async move {
        tokio::select! {
            biased;
            () = async { loop { if *detached.borrow_and_update() || detached.changed().await.is_err() { break; } } } => {
                match attachment.release().await {
                    Ok(()) => { done.send_replace(true); }
                    Err(error) => { tracing::warn!(%error, "gateway planned detach not confirmed"); }
                }
                return;
            }
            result = attachment.lost() => {
                if let Err(error) = result { tracing::warn!(%error, "gateway ownership transport failed"); }
                if let Some(live) = weak.upgrade() { live.revoke(); }
            }
            () = async { loop { if *cancel.borrow_and_update() || cancel.changed().await.is_err() { break; } } } => (),
        }
        drop(attachment);
        done.send_replace(true);
    });
    Ok(())
}

/// Non-cloneable current authority. Dropping it revokes all old/new generation work.
pub struct Owner {
    live: Arc<Live>,
    epoch: u64,
}
impl Owner {
    pub fn credential(&self) -> &Secret {
        &self.live.credential
    }
    pub fn policy(&self) -> &Policy {
        &self.live.policy
    }
    /// A candidate is not an owner. A traffic credential cannot construct one.
    pub fn prepare(&self) -> Result<Candidate, Error> {
        self.live.check(self.epoch)?;
        Ok(Candidate {
            live: self.live.clone(),
            epoch: self.epoch,
        })
    }
    /// Monitor the already-authenticated launcher's host-only connection, without leases/heartbeats.
    /// No descriptor is delivered to runtime children. EOF is revocation, never worker exit evidence.
    pub async fn observe(self, mut stream: impl AsyncRead + Unpin) -> Result<(), Error> {
        let mut byte = [0];
        let mut cancel = self.live.cancel.subscribe();
        tokio::select! {
            result = stream.read(&mut byte) => { result?; }
            () = async { loop { if *cancel.borrow_and_update() || cancel.changed().await.is_err() { break; } } } => (),
        }
        Ok(())
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        match self.live.state.lock() {
            Ok(mut state) if state.epoch == self.epoch => {
                state.revoked = true;
                self.live.cancel.send_replace(true);
            }
            Ok(_) => (),
            Err(error) => {
                self.live.cancel.send_replace(true);
                tracing::error!(%error, "host gateway owner release failed");
            }
        }
    }
}

/// Consume only after authenticating the candidate launcher control channel through private host control.
pub struct Candidate {
    live: Arc<Live>,
    epoch: u64,
}
impl Candidate {
    pub fn confirm(self, former: &Owner) -> Result<Owner, Error> {
        if !Arc::ptr_eq(&self.live, &former.live) || self.epoch != former.epoch {
            return Err(Error::Denied);
        }
        let epoch = {
            let mut state = self.live.state.lock().map_err(|_| Error::Poisoned)?;
            if state.revoked || state.epoch != self.epoch {
                return Err(Error::Denied);
            }
            state.epoch = state.epoch.checked_add(1).ok_or(Error::Capacity)?;
            state.epoch
        };
        Ok(Owner {
            live: self.live,
            epoch,
        })
    }
}

#[cfg(test)]
mod test;
