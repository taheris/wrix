//! Native terminal evidence drives retirement; no elapsed lease, quiet socket or requester status does.
mod runtime;
use super::{Delivery, Error, Owner, Record, State, check_directory};
use crate::deployment;
use std::{
    fs, io,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};
use wrix_core::{nix_store::pool::Key, path::WorkspaceHash};
use wrix_state::{execution::Ticket, host::Host};

struct Worker {
    pool: Key,
    ticket: Ticket,
    handle: JoinHandle<()>,
}
#[derive(Default)]
pub struct Supervisor(Mutex<Vec<Worker>>);
const LIMIT: usize = 128;

impl Supervisor {
    /// Reconcile only recorded protection; no startup path starts, resumes or adopts a pool runtime.
    pub(crate) fn recover_all(
        &self,
        host: &Arc<Host>,
        deployment: &deployment::Id,
    ) -> Result<(), Error> {
        let root = host.roots().state().join("control/nix");
        match fs::symlink_metadata(&root) {
            Ok(_) => crate::nix_store::owner::check_parent(&root)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let repository = wrix_core::nix_store::pool::Repository::try_from(
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| Error::Invalid)?,
            )
            .map_err(|_| Error::Invalid)?;
            let pools = crate::nix_store::owner::inspect(host, &repository, Some(deployment))?;
            for (pool, _) in pools {
                self.recover_control(
                    host,
                    deployment,
                    &pool,
                    &entry.path().join(pool.system.as_str()),
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn start(
        &self,
        host: Arc<Host>,
        deployment: deployment::Id,
        namespace: WorkspaceHash,
        delivery: &Delivery,
    ) -> Result<(), Error> {
        self.spawn(
            host,
            deployment,
            namespace,
            delivery.pool.clone(),
            delivery.ticket.clone(),
        )
    }

    fn spawn(
        &self,
        host: Arc<Host>,
        deployment: deployment::Id,
        namespace: WorkspaceHash,
        pool: Key,
        ticket: Ticket,
    ) -> Result<(), Error> {
        let mut workers = self.0.lock().map_err(|_| Error::Invalid)?;
        let mut active = Vec::new();
        for worker in workers.drain(..) {
            if worker.handle.is_finished() {
                if worker.handle.join().is_err() {
                    tracing::warn!(
                        operation = "nix_execution_retire",
                        "retirement supervisor panicked; protection is retained"
                    );
                }
            } else {
                active.push(worker);
            }
        }
        *workers = active;
        if workers
            .iter()
            .any(|worker| worker.pool == pool && worker.ticket == ticket)
        {
            return Ok(());
        }
        if workers.len() >= LIMIT {
            return Err(Error::Busy);
        }
        let bound_pool = pool.clone();
        let bound_ticket = ticket.clone();
        let handle = thread::Builder::new().name("wrix-pool-retire".to_owned()).spawn(move || {
            if let Err(error) = watch(&host, &deployment, &namespace, &bound_pool, &bound_ticket) { tracing::warn!(operation = "nix_execution_retire", repository = %bound_pool.repository, system = bound_pool.system.as_str(), %error, "retirement is uncertain; native protection is retained"); }
        })?;
        workers.push(Worker {
            pool,
            ticket,
            handle,
        });
        Ok(())
    }

    /// Reattach only terminal-evidence observation; never reconstruct a grant, mount or missing runtime.
    pub(crate) fn recover(
        &self,
        host: &Arc<Host>,
        deployment: &deployment::Id,
        owner: &Owner,
    ) -> Result<(), Error> {
        self.recover_control(
            host,
            deployment,
            owner.pool(),
            &owner.prepared.reservation.paths.control,
        )
    }

    fn recover_control(
        &self,
        host: &Arc<Host>,
        deployment: &deployment::Id,
        pool: &Key,
        control: &std::path::Path,
    ) -> Result<(), Error> {
        let ledger = crate::nix_store::owner::backend::Record::read(control, deployment, pool)?;
        super::protected(control, pool, &ledger.executions)?;
        let root = control.join("executions");
        match fs::symlink_metadata(&root) {
            Ok(_) => check_directory(&root)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let record = Record::read(&entry.path())?;
            let ticket = Ticket::parse(entry.file_name().to_str().ok_or(Error::Invalid)?)?;
            if record.ticket != ticket || record.pool != *pool {
                return Err(Error::Invalid);
            }
            if matches!(
                record.state,
                State::Attached { .. } | State::Detaching { .. } | State::Detached
            ) {
                let namespace =
                    WorkspaceHash::parse(record.namespace.as_str()).map_err(|_| Error::Invalid)?;
                self.spawn(
                    host.clone(),
                    deployment.clone(),
                    namespace,
                    record.pool,
                    record.ticket,
                )?;
            }
        }
        Ok(())
    }
}

fn watch(
    host: &Arc<Host>,
    deployment: &deployment::Id,
    namespace: &WorkspaceHash,
    pool: &Key,
    ticket: &Ticket,
) -> Result<(), Error> {
    loop {
        if let Some(proof) = wrix_state::execution::terminal(host, namespace, ticket)? {
            match Owner::open(host, deployment, pool)
                .and_then(|mut owner| owner.release_execution(&proof))
            {
                Ok(()) => return Ok(()),
                Err(Error::Busy) => (),
                Err(error) => return Err(error),
            }
            thread::sleep(Duration::from_secs(2));
        } else {
            thread::sleep(runtime::reconcile(
                host, deployment, namespace, pool, ticket,
            )?);
        }
    }
}
