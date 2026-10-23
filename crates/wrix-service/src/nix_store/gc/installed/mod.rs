//! Native collection observes existing recorded owners; it never realizes or starts software.

use super::{Attempt, Backend};
use crate::nix_store::owner::{
    self,
    backend::{Error, Owner},
};
use wrix_core::nix_store::pool::{Key, Repository};
use wrix_state::{context::Selection, host::Host};

pub struct Existing<'a> {
    selection: &'a Selection,
    host: Host,
    deployment: crate::deployment::Id,
}

pub struct Completed {
    _owner: Owner,
    outcome: Result<(), Error>,
}

impl<'a> Existing<'a> {
    pub(crate) fn open(selection: &'a Selection) -> Result<Self, Error> {
        selection.recheck().map_err(std::io::Error::other)?;
        let host = Host::discover().map_err(std::io::Error::other)?;
        let deployment = crate::control::existing_deployment(&host)
            .map_err(std::io::Error::other)?
            .ok_or(Error::Unavailable)?;
        Ok(Self {
            selection,
            host,
            deployment,
        })
    }

    fn check(&self, repository: &Repository) -> Result<(), Error> {
        self.selection.recheck().map_err(std::io::Error::other)?;
        match wrix_state::nix_store::inspect(self.selection).map_err(std::io::Error::other)? {
            wrix_state::nix_store::Approval::Bound {
                repository: current,
                ..
            } if &current == repository => Ok(()),
            _ => Err(Error::Invalid),
        }
    }
}

impl Backend for Existing<'_> {
    type Guard = Completed;
    type Error = Error;

    fn pools(&self, repository: &Repository) -> Result<Vec<Key>, Error> {
        self.check(repository)?;
        let pools = owner::inspect(&self.host, repository, Some(&self.deployment))?;
        if pools.is_empty() {
            return Err(Error::Unavailable);
        }
        Ok(pools.into_iter().map(|(pool, _phase)| pool).collect())
    }

    fn acquire(&self, pool: &Key) -> Attempt<Completed, Error> {
        let result = self
            .check(&pool.repository)
            .and_then(|()| Owner::open(&self.host, &self.deployment, pool))
            .and_then(|owner| {
                let outcome = match owner.collect() {
                    Ok(()) => Ok(()),
                    Err(Error::Collection) => Err(Error::Collection),
                    Err(error) => return Err(error),
                };
                Ok(Completed {
                    _owner: owner,
                    outcome,
                })
            });
        match result {
            Ok(guard) => Attempt::Acquired(guard),
            Err(Error::Busy) => Attempt::Busy,
            Err(error) => Attempt::Unavailable(error),
        }
    }

    fn collect(&self, guard: Completed) -> Result<(), Error> {
        guard.outcome
    }
}
