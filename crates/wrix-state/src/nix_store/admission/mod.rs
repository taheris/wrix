//! A short host issuance window, not a native owner, endpoint, filesystem export or live-work lease.

pub mod request;

use super::{
    Error,
    record::{Catalog, Common},
};
use crate::{
    context::{Context, Selection},
    host::Host,
    store::Store,
};
use wrix_core::{
    nix_store::pool::{Key, System},
    path::WorkspaceHash,
};

/// Retains original checked scope and excludes approval writers only through local issuance.
/// Drop before native work or long-lived I/O; runtime lifetime needs independent pool protection.
pub struct Guard {
    store: Store,
    host: Host,
    context: Context,
    common: Common,
    pool: Key,
}

impl Guard {
    /// Synchronous host issuance uses current explicit membership, never a caller-selected pool key.
    pub fn acquire(selection: &Selection, system: System) -> Result<Self, Error> {
        let host = Host::discover()?;
        let context = selection.context().ok_or(Error::ApprovalRequired)?;
        context.recheck(&host)?;
        let store = Store::mutate(&host, false)?;
        store.connection.execute_batch("BEGIN IMMEDIATE")?;
        context.check_store(&host, Some(&store))?;
        let common = Common::observe(context)?;
        let catalog = Catalog::load(&store)?;
        let record = catalog
            .selected(context, &common)?
            .filter(|record| record.enabled)
            .ok_or(Error::ApprovalRequired)?;
        let pool = Key {
            repository: record.repository.clone(),
            system,
        };
        let admission = Self {
            store,
            host,
            context: context.clone(),
            common,
            pool,
        };
        admission.recheck()?;
        Ok(admission)
    }

    pub const fn pool(&self) -> &Key {
        &self.pool
    }

    pub const fn namespace(&self) -> &WorkspaceHash {
        self.context.key()
    }

    /// Recheck the retained context, never a newly discovered workspace/hash surrogate.
    pub fn recheck(&self) -> Result<(), Error> {
        self.host.check()?;
        self.context.recheck(&self.host)?;
        self.context.check_store(&self.host, Some(&self.store))?;
        if Common::observe(&self.context)? != self.common {
            return Err(Error::RepositoryChanged);
        }
        let catalog = Catalog::load(&self.store)?;
        let record = catalog
            .selected(&self.context, &self.common)?
            .filter(|record| record.enabled)
            .ok_or(Error::ApprovalRequired)?;
        if record.repository != self.pool.repository {
            return Err(Error::Inconsistent);
        }
        Ok(())
    }
}
