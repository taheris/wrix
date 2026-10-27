//! Existing private bindings authorize observation only, never new work or scope rediscovery.
use super::super::Removal;
use super::{Error, Host, Identity, Phase, Store, Ticket};
use crate::store::Generation;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use wrix_core::path::WorkspaceHash;

/// A generation-fenced existing immutable object, not admission or permission to start one.
pub struct Recorded {
    host: Arc<Host>,
    namespace: WorkspaceHash,
    ticket: Ticket,
    identity: Identity,
    generation: Generation,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u16,
    identity: Identity,
}
impl Recorded {
    pub fn load(
        host: Arc<Host>,
        namespace: &WorkspaceHash,
        ticket: &Ticket,
        generation: &Generation,
    ) -> Result<Option<Self>, Error> {
        let store = Store::inspect(&host)?.ok_or(Error::Uninitialized)?;
        if &store.generation()? != generation {
            return Err(Error::Conflict);
        }
        let identity = match super::super::read_namespace(&store, namespace, ticket)? {
            Some(Phase::AppleBound(id) | Phase::AppleRetired(id)) => id,
            Some(
                Phase::Reserved
                | Phase::Launching
                | Phase::Cancelled
                | Phase::Bound(_)
                | Phase::Retired(_),
            ) => return Ok(None),
            None => return Err(Error::Conflict),
        };
        let binding = Self {
            host,
            namespace: namespace.clone(),
            ticket: ticket.clone(),
            identity,
            generation: generation.clone(),
        };
        binding.read(&store)?;
        Ok(Some(binding))
    }
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn status(&self) -> Result<Removal, Error> {
        let store = Store::inspect(&self.host)?.ok_or(Error::Uninitialized)?;
        self.read(&store)
    }
    fn owner(&self) -> String {
        format!("{}/apple-removal", self.ticket.owner())
    }
    fn read(&self, store: &Store) -> Result<Removal, Error> {
        if store.generation()? != self.generation {
            return Err(Error::Conflict);
        }
        let bytes: Option<Vec<u8>> = store
            .connection
            .query_row(
                "SELECT value FROM namespace_metadata WHERE native_key = ?1 AND owner = ?2",
                params![self.namespace.as_str(), self.owner()],
                |row| row.get(0),
            )
            .optional()?;
        let intent = bytes
            .map(|bytes| serde_json::from_slice::<Intent>(&bytes).map_err(|_| Error::Unsupported))
            .transpose()?;
        if intent
            .as_ref()
            .is_some_and(|intent| intent.version != 1 || intent.identity != self.identity)
        {
            return Err(Error::Conflict);
        }
        match super::super::read_namespace(store, &self.namespace, &self.ticket)? {
            Some(Phase::AppleRetired(id)) if id == self.identity => Ok(Removal::Retired),
            Some(Phase::AppleBound(id)) if id == self.identity => Ok(if intent.is_some() {
                Removal::Removing
            } else {
                Removal::Bound
            }),
            _ => Err(Error::Conflict),
        }
    }
    /// Called only after exact native inactivity; intent precedes graceful removal, not vice versa.
    pub fn removing(&self) -> Result<(), Error> {
        self.update(Removal::Removing)
    }
    /// Called only after independently confirmed native absence in the same recorded runtime namespace.
    pub fn retire(&self) -> Result<(), Error> {
        self.update(Removal::Retired)
    }
    fn update(&self, next: Removal) -> Result<(), Error> {
        let store = Store::mutate(&self.host, false)?;
        store.connection.execute_batch("BEGIN IMMEDIATE")?;
        let current = self.read(&store)?;
        if current == Removal::Retired {
            return Ok(());
        }
        match next {
            Removal::Removing if current == Removal::Bound => {
                let intent = Intent {
                    version: 1,
                    identity: self.identity.clone(),
                };
                store.connection.execute(
                    "INSERT INTO namespace_metadata VALUES (?1, ?2, ?3)",
                    params![
                        self.namespace.as_str(),
                        self.owner(),
                        serde_json::to_vec(&intent).map_err(|_| Error::Unsupported)?
                    ],
                )?;
            }
            Removal::Removing => (),
            Removal::Retired if current == Removal::Removing => {
                store.connection.execute(
                    "UPDATE namespace_metadata SET value = ?3 WHERE native_key = ?1 AND owner = ?2",
                    params![
                        self.namespace.as_str(),
                        self.ticket.owner(),
                        serde_json::to_vec(&Phase::AppleRetired(self.identity.clone()))
                            .map_err(|_| Error::Unsupported)?
                    ],
                )?;
                store.connection.execute(
                    "DELETE FROM namespace_metadata WHERE native_key = ?1 AND owner = ?2",
                    params![self.namespace.as_str(), self.owner()],
                )?;
            }
            _ => return Err(Error::Conflict),
        }
        store.connection.execute_batch("COMMIT")?;
        Ok(())
    }
}
