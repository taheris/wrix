//! Durable inactive-removal intent closes the remove/terminal crash window; absence alone is never exit evidence.
use super::{Container, Error, Inspect, Observation, inactive};
use std::process::Output;
use wrix_state::execution::{Recorded, Removal};

/// Closed native operations only; implementations select and bound the recorded runtime, not caller commands.
pub enum Operation<'a> {
    Inspect(&'a Container),
    Remove(&'a Container),
    Exists(&'a Container),
}
pub trait Runtime {
    fn query(&mut self, operation: Operation<'_>) -> Result<Output, Error>;
}

pub(super) fn with_command(
    record: &Recorded,
    template: &std::process::Command,
    environment: super::Environment,
) -> Result<Observation, Error> {
    struct Commands<'a> {
        template: &'a std::process::Command,
        environment: super::Environment,
    }
    impl Runtime for Commands<'_> {
        fn query(&mut self, operation: Operation<'_>) -> Result<Output, Error> {
            let mut command = super::runtime_command(self.template, self.environment);
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
            Ok(command.output()?)
        }
    }
    reconcile(
        record,
        &mut Commands {
            template,
            environment,
        },
    )
}

fn absent(runtime: &mut impl Runtime, id: &Container) -> Result<bool, Error> {
    let output = runtime.query(Operation::Exists(id))?;
    match (output.status.code(), output.stdout.is_empty()) {
        (Some(1), true) => Ok(true),
        (Some(0), true) => Ok(false),
        _ => Err(Error::Unknown),
    }
}
pub fn reconcile(record: &Recorded, runtime: &mut impl Runtime) -> Result<Observation, Error> {
    match record.status()? {
        Removal::Retired => return Ok(Observation::Retired),
        Removal::Removing if absent(runtime, record.container())? => {
            record.retire()?;
            return Ok(Observation::Retired);
        }
        Removal::Bound | Removal::Removing => (),
    }
    let output = runtime.query(Operation::Inspect(record.container()))?;
    if !output.status.success() {
        return match record.status()? {
            Removal::Retired => Ok(Observation::Retired),
            Removal::Removing if absent(runtime, record.container())? => {
                record.retire()?;
                Ok(Observation::Retired)
            }
            _ => Err(Error::Unknown),
        };
    }
    let objects: Vec<Inspect> = serde_json::from_slice(&output.stdout)?;
    if !inactive(&objects, record.container()) {
        return Ok(if record.status()? == Removal::Retired {
            Observation::Retired
        } else {
            Observation::Protected
        });
    }
    record.removing()?;
    if record.status()? == Removal::Retired {
        return Ok(Observation::Retired);
    }
    let removed = runtime.query(Operation::Remove(record.container()))?;
    if !removed.status.success()
        || String::from_utf8_lossy(&removed.stdout).trim() != record.container().as_str()
    {
        return if record.status()? == Removal::Retired {
            Ok(Observation::Retired)
        } else {
            Err(Error::Unknown)
        };
    }
    if !absent(runtime, record.container())? {
        return Err(Error::Unknown);
    }
    record.retire()?;
    Ok(Observation::Retired)
}
