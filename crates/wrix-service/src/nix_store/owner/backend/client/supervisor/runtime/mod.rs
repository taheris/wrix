//! Observe existing Podman bindings using the protected owner's recorded package and bounded native operations.
use super::{Error, Host, Key, Ticket, WorkspaceHash, deployment};
use crate::nix_store::owner::backend::{self, Program, tools::Tools};
use std::{io, sync::Arc, time::Duration};
use wrix_sandbox::execution::{
    Observation,
    retirement::{self, Operation},
};

struct Native {
    tools: Tools,
}
impl retirement::Runtime for Native {
    fn query(
        &mut self,
        operation: Operation<'_>,
    ) -> Result<std::process::Output, wrix_sandbox::execution::Error> {
        let mut command = self.tools.command(Program::Podman);
        match operation {
            Operation::Inspect(id) => {
                command.args(["inspect", "--type", "container", id.as_str()]);
            }
            Operation::Remove(id) => {
                command.args(["rm", id.as_str()]);
            }
            Operation::Exists(id) => {
                command.args(["container", "exists", id.as_str()]);
            }
        }
        self.tools
            .run(command, None, Some(backend::COMMAND_BUDGET))
            .map_err(|error| wrix_sandbox::execution::Error::Io(io::Error::other(error)))
    }
}
pub(super) fn reconcile(
    host: &Arc<Host>,
    deployment: &deployment::Id,
    namespace: &WorkspaceHash,
    pool: &Key,
    ticket: &Ticket,
) -> Result<Duration, Error> {
    let control = host
        .roots()
        .state()
        .join("control/nix")
        .join(pool.repository.to_string())
        .join(pool.system.as_str());
    let client = super::super::Record::read(&control.join("executions").join(ticket.as_str()))?;
    if client.pool != *pool
        || client.ticket != *ticket
        || client.namespace.as_str() != namespace.as_str()
    {
        return Err(Error::Invalid);
    }
    if !client.plane.is_podman_namespace() {
        return Ok(Duration::from_secs(2));
    }
    let generation = wrix_state::store::Generation::parse(&client.generation)?;
    let Some(record) =
        wrix_state::execution::Recorded::load(host.clone(), namespace, ticket, &generation)?
    else {
        return Ok(Duration::from_secs(2));
    };
    if record.status()? == wrix_state::execution::Removal::Retired {
        return Ok(Duration::from_secs(2));
    }
    let ledger = backend::Record::read(&control, deployment, pool)?;
    if !ledger.executions.contains(ticket) {
        return Err(Error::Retained);
    }
    let backend::State::Serving { connection } = ledger.state else {
        return Err(Error::Retained);
    };
    let mut runtime = Native {
        tools: connection.target.attempt.software.tools,
    };
    match retirement::reconcile(&record, &mut runtime) {
        Ok(Observation::Protected | Observation::Retired) => Ok(Duration::from_secs(2)),
        Err(error) => {
            tracing::warn!(operation = "nix_execution_observe", ticket = ticket.as_str(), %error, "native containment observation is uncertain; protection is retained");
            Ok(Duration::from_secs(15)) // Retry observation only; elapsed time never grants removal or release.
        }
    }
}
