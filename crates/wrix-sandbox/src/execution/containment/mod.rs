//! Checked shared containment: original approved handoff, exact native object, then start authority.
use super::{Environment, Error, Observation, Phase, Projection, ProjectionMount};
use std::process::{Child, Command, Stdio};
use wrix_state::execution::{Bound, Handoff};

/// A foreground launcher, not terminal evidence. Drop never removes native work.
pub struct Running {
    prepared: Prepared,
    child: Child,
}
impl Running {
    pub const fn child(&mut self) -> &mut Child {
        &mut self.child
    }
    pub fn reconcile(&self) -> Result<Observation, Error> {
        self.prepared.reconcile()
    }
}

/// Exact image and read-only delivery are checked after binding and again before one-use start.
/// Drop never removes a container or manufactures terminal evidence.
#[must_use]
pub struct Prepared {
    bound: Bound,
    runtime: Command,
    environment: Environment,
    start: Command,
    image: crate::image::Digest,
    mounts: Vec<ProjectionMount>,
}
impl Prepared {
    pub fn create(
        handoff: Handoff,
        command: &Command,
        interactive: bool,
        image: &crate::image::Digest,
        mounts: &[ProjectionMount],
    ) -> Result<Self, Error> {
        Self::prepare(
            handoff,
            command,
            interactive,
            image,
            mounts,
            Environment::Inherited,
        )
    }
    /// Package-selected callers carry an explicit environment; never inherit ambient runtime selectors.
    pub fn create_fixed(
        handoff: Handoff,
        command: &Command,
        interactive: bool,
        image: &crate::image::Digest,
        mounts: &[ProjectionMount],
    ) -> Result<Self, Error> {
        Self::prepare(
            handoff,
            command,
            interactive,
            image,
            mounts,
            Environment::Fixed,
        )
    }
    fn prepare(
        handoff: Handoff,
        command: &Command,
        interactive: bool,
        image: &crate::image::Digest,
        mounts: &[ProjectionMount],
        environment: Environment,
    ) -> Result<Self, Error> {
        let id = super::create(command, environment)?;
        let bound = handoff.bind(id)?;
        let runtime = super::runtime_command(command, environment);
        let projection = super::projection_for(&runtime, environment, bound.container())?;
        verify(&projection, image, mounts)?;
        bound.recheck()?;
        let start = super::start(command, bound.container(), interactive, environment);
        Ok(Self {
            bound,
            runtime,
            environment,
            start,
            image: image.clone(),
            mounts: mounts.to_vec(),
        })
    }
    pub fn stdio(mut self, stdin: Stdio, stdout: Stdio, stderr: Stdio) -> Self {
        self.start.stdin(stdin).stdout(stdout).stderr(stderr);
        self
    }
    pub fn spawn(mut self) -> Result<Running, Error> {
        if !matches!(self.bound.observe()?, Phase::Bound(_)) {
            return Err(Error::Unknown);
        }
        verify(
            &super::projection_for(&self.runtime, self.environment, self.bound.container())?,
            &self.image,
            &self.mounts,
        )?;
        self.bound.recheck()?;
        let child = self.start.spawn()?;
        Ok(Running {
            prepared: self,
            child,
        })
    }
    pub const fn ticket(&self) -> &wrix_state::execution::Ticket {
        self.bound.ticket()
    }
    pub fn reconcile(&self) -> Result<Observation, Error> {
        if matches!(self.bound.observe()?, Phase::Retired(_)) {
            return Ok(Observation::Retired);
        }
        let observation = super::remove(
            &self.runtime,
            self.environment,
            self.bound.container(),
            || {
                self.bound.recheck()?;
                Ok(())
            },
        )?;
        if observation == Observation::Retired {
            self.bound.retire()?;
        }
        Ok(observation)
    }
}

fn verify(
    projection: &Projection,
    image: &crate::image::Digest,
    mounts: &[ProjectionMount],
) -> Result<(), Error> {
    if &projection.image != image || mounts.is_empty() {
        return Err(Error::Delivery);
    }
    for (index, expected) in mounts.iter().enumerate() {
        if expected.writable
            || !expected.source.is_absolute()
            || !expected.destination.is_absolute()
            || expected
                .source
                .components()
                .chain(expected.destination.components())
                .any(|component| matches!(component, std::path::Component::ParentDir))
            || mounts[..index]
                .iter()
                .any(|previous| previous.destination == expected.destination)
        {
            return Err(Error::Delivery);
        }
        let mut overlaps = projection.mounts.iter().filter(|actual| {
            actual.destination.starts_with(&expected.destination)
                || expected.destination.starts_with(&actual.destination)
        });
        let actual = overlaps.next().ok_or(Error::Delivery)?;
        if overlaps.next().is_some()
            || actual.writable
            || actual.source != expected.source
            || actual.destination != expected.destination
        {
            return Err(Error::Delivery);
        }
    }
    Ok(())
}

#[cfg(test)]
mod test;
