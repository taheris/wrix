//! One-use approved native handoff, retaining the original checked context through binding and retirement.
use super::{Container, Context, Error, Host, Phase, Store, Ticket};
use crate::store::Generation;
use rusqlite::params;
use std::sync::Arc;

/// Approval and reservation were consumed atomically; dropping this never asserts native teardown.
pub struct Launching {
    host: Arc<Host>,
    generation: Generation,
    context: Context,
    ticket: Ticket,
}
/// One immutable native object bound under the original handoff, not a workspace rediscovery.
pub struct Bound {
    host: Arc<Host>,
    generation: Generation,
    context: Context,
    ticket: Ticket,
    container: Container,
}
impl Launching {
    pub const fn ticket(&self) -> &Ticket {
        &self.ticket
    }

    pub(crate) fn commit(
        host: Host,
        context: Context,
        ticket: Ticket,
        store: Store,
    ) -> Result<Self, Error> {
        context.check_store(&host, Some(&store))?;
        super::require_reserved(&store, &context, &ticket)?;
        store.connection.execute(
            "UPDATE namespace_metadata SET value = ?3 WHERE native_key = ?1 AND owner = ?2",
            params![
                context.key().as_str(),
                ticket.owner(),
                serde_json::to_vec(&Phase::Launching).map_err(|_| Error::Unsupported)?
            ],
        )?;
        let generation = store.generation()?;
        store.connection.execute_batch("COMMIT")?;
        drop(store);
        Ok(Self {
            host: Arc::new(host),
            generation,
            context,
            ticket,
        })
    }
    pub fn bind_apple(
        self,
        identity: super::apple::Identity,
    ) -> Result<super::apple::Bound, Error> {
        super::transition(
            &self.host,
            &self.context,
            &self.ticket,
            &Phase::Launching,
            &Phase::AppleBound(identity.clone()),
        )?;
        Ok(super::apple::Bound::new(
            self.host,
            self.generation,
            self.context,
            self.ticket,
            identity,
        ))
    }
    pub fn bind(self, container: Container) -> Result<Bound, Error> {
        super::bind(&self.host, &self.context, &self.ticket, &container)?;
        Ok(Bound {
            host: self.host,
            generation: self.generation,
            context: self.context,
            ticket: self.ticket,
            container,
        })
    }
}
impl Bound {
    /// Observe already-bound native work even after admission withdrawal; this can never authorize restart.
    pub fn recorded(&self) -> Result<super::Recorded, Error> {
        let record = super::Recorded::load(
            self.host.clone(),
            self.context.key(),
            &self.ticket,
            &self.generation,
        )?
        .ok_or(Error::Conflict)?;
        if record.container() != &self.container {
            return Err(Error::Conflict);
        }
        Ok(record)
    }
    pub const fn ticket(&self) -> &Ticket {
        &self.ticket
    }
    pub const fn container(&self) -> &Container {
        &self.container
    }
    pub fn recheck(&self) -> Result<(), Error> {
        self.context.recheck(&self.host)
    }
    pub fn observe(&self) -> Result<Phase, Error> {
        let phase =
            super::observe(&self.host, &self.context, &self.ticket)?.ok_or(Error::Conflict)?;
        match &phase {
            Phase::Bound(id) | Phase::Retired(id) if id == &self.container => Ok(phase),
            _ => Err(Error::Conflict),
        }
    }
    /// Caller must independently establish exact inactive removal and confirmed absence first.
    pub fn retire(&self) -> Result<(), Error> {
        super::retire(&self.host, &self.context, &self.ticket, &self.container)
    }
}
