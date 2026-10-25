//! Trusted host factories supply only execution-bound Podman delivery, never worker-selected endpoints or authority.
use std::{num::NonZeroU16, os::unix::ffi::OsStrExt, path::PathBuf};
use wrix_core::nix_store::{
    binding::Binding,
    pool::{Key, System},
};
use wrix_state::{
    context::Selection,
    execution::{self, Phase, Ticket},
    host::Host,
};

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// shared Nix delivery is unavailable; provide a compatible host package/controller and inspect retained ownership
    Unavailable,
    /// shared Nix host I/O failed: {0}
    Io(#[from] std::io::Error),
    /// shared Nix workspace authority failed: {0}
    State(#[from] wrix_state::store::Error),
    /// shared Nix approval failed: {0}
    Approval(#[from] wrix_state::nix_store::Error),
    /// shared Nix host capability failed: {0}
    Host(#[from] wrix_state::host::Error),
    /// shared Nix native containment failed: {0}
    Containment(#[from] crate::execution::Error),
    /// shared Nix preparation changed its fixed delivery; ownership and protection are retained
    Delivery,
}

/// Private manager-namespace sources; never traverse/copy the live store through the host namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delivery {
    pool: Key,
    ticket: Ticket,
    store: PathBuf,
    configuration: PathBuf,
    port: NonZeroU16,
}
impl Delivery {
    /// Derive fixed service-private sources, not a caller-chosen mount root. This description grants no admission.
    pub fn podman(pool: Key, ticket: Ticket, port: NonZeroU16) -> Result<Self, Error> {
        let host = Host::discover()?;
        let root = host
            .roots()
            .state()
            .join("control/nix")
            .join(pool.repository.to_string())
            .join(pool.system.as_str())
            .join("executions")
            .join(ticket.as_str());
        let store = root.join("store");
        let configuration = root.join("worker");
        for path in [&store, &configuration] {
            if !path.is_absolute()
                || path.as_os_str().as_bytes().contains(&b':')
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err(Error::Delivery);
            }
        }
        Ok(Self {
            pool,
            ticket,
            store,
            configuration,
            port,
        })
    }
    pub fn mounts(&self) -> [crate::execution::ProjectionMount; 3] {
        [
            crate::execution::ProjectionMount {
                source: self.store.clone(),
                destination: "/nix/store".into(),
                writable: false,
            },
            crate::execution::ProjectionMount {
                source: self.configuration.clone(),
                destination: "/run/wrix/nix-pool".into(),
                writable: false,
            },
            crate::execution::ProjectionMount {
                source: self.configuration.join("nix.conf"),
                destination: "/etc/nix/nix.conf".into(),
                writable: false,
            },
        ]
    }
    pub fn local_endpoint(&self) -> String {
        format!("169.254.1.2:{}/tcp", self.port)
    }
    pub fn configure(&self, command: &mut std::process::Command) {
        for mount in self.mounts() {
            let mut argument = mount.source.into_os_string();
            argument.push(":");
            argument.push(mount.destination);
            argument.push(":ro");
            command.arg("-v").arg(argument);
        }
        command.arg("-e").arg(format!(
            "WRIX_NETWORK_LOCAL_ENDPOINTS={}",
            self.local_endpoint()
        ));
    }
}
/// Re-observe native export identity and fixed configuration, not historical job readiness.
pub trait Session {
    fn delivery(&self) -> Result<Delivery, Error>;
    fn podman(&self) -> Result<std::process::Command, Error>;
}
pub trait Factory {
    fn admit(&self) -> Result<(), Error>;
    fn prepare_podman(
        &self,
        selection: &Selection,
        system: System,
        ticket: &Ticket,
        image: &Binding,
    ) -> Result<Box<dyn Session>, Error>;
}
pub struct Unavailable;
impl Factory for Unavailable {
    fn admit(&self) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
    fn prepare_podman(
        &self,
        _: &Selection,
        _: System,
        _: &Ticket,
        _: &Binding,
    ) -> Result<Box<dyn Session>, Error> {
        Err(Error::Unavailable)
    }
}

/// A never-handed-off reservation is cancelled on ordinary setup failure; uncertainty is never native exit proof.
pub struct Attempt {
    host: Host,
    context: wrix_state::context::Context,
    selection: Selection,
    ticket: Ticket,
    system: System,
    image: Binding,
}
impl Drop for Attempt {
    fn drop(&mut self) {
        let context = &self.context;
        match execution::observe(&self.host, context, &self.ticket) {
            Ok(Some(Phase::Reserved)) => {
                if let Err(error) = execution::cancel(&self.host, context, &self.ticket) {
                    tracing::warn!(%error, ticket = %self.ticket.as_str(), "never-started shared reservation remains protected");
                }
            }
            Ok(_) => (), // Launching/Bound remain protected; retirement belongs to independent native observation.
            Err(error) => {
                tracing::warn!(%error, ticket = %self.ticket.as_str(), "shared reservation scope is uncertain; protection is retained");
            }
        }
    }
}
impl Attempt {
    pub fn reserve(
        selection: &Selection,
        system: System,
        image: Binding,
        ticket: Option<&Ticket>,
    ) -> Result<Self, Error> {
        let host = Host::discover()?;
        selection.authorize_effects()?;
        let context = selection.context().ok_or(Error::Unavailable)?;
        let ticket = match ticket {
            Some(ticket)
                if execution::observe(&host, context, ticket)? == Some(Phase::Reserved) =>
            {
                ticket.clone()
            }
            Some(_) => return Err(Error::Delivery),
            None => execution::reserve(&host, context)?,
        };
        Ok(Self {
            host,
            context: context.clone(),
            selection: selection.clone(),
            ticket,
            system,
            image,
        })
    }
    pub const fn ticket(&self) -> &Ticket {
        &self.ticket
    }
    pub fn prepare(self, factory: &dyn Factory) -> Result<Prepared, Error> {
        let pool = wrix_state::nix_store::admission::Guard::acquire(&self.selection, self.system)?
            .pool()
            .clone();
        let session =
            factory.prepare_podman(&self.selection, self.system, &self.ticket, &self.image)?;
        let delivery = session.delivery()?;
        if delivery.pool != pool || delivery.ticket != self.ticket {
            return Err(Error::Delivery);
        }
        Ok(Prepared {
            attempt: self,
            session,
            delivery,
        })
    }
}
pub struct Prepared {
    attempt: Attempt,
    session: Box<dyn Session>,
    delivery: Delivery,
}
impl Prepared {
    pub const fn delivery(&self) -> &Delivery {
        &self.delivery
    }
    pub fn command(&self) -> Result<std::process::Command, Error> {
        self.session.podman()
    }
    pub const fn image(&self) -> &Binding {
        &self.attempt.image
    }
    /// Recheck live delivery, then atomically commit original approval and reservation before native creation.
    pub fn spawn(
        self,
        command: &std::process::Command,
        interactive: bool,
    ) -> Result<crate::execution::Running, Error> {
        if self.session.delivery()? != self.delivery {
            return Err(Error::Delivery);
        }
        let runtime = self.session.podman()?;
        if command.get_program() != runtime.get_program()
            || command.get_current_dir() != runtime.get_current_dir()
            || !command.get_envs().eq(runtime.get_envs())
        {
            return Err(Error::Delivery);
        }
        let guard = wrix_state::nix_store::admission::Guard::acquire(
            &self.attempt.selection,
            self.attempt.system,
        )?;
        if guard.pool() != &self.delivery.pool {
            return Err(Error::Delivery);
        }
        let handoff = guard.handoff(self.attempt.ticket.clone())?;
        let image = crate::image::Digest::parse(self.attempt.image.digest().as_str())
            .map_err(|_| Error::Delivery)?;
        let prepared = crate::execution::Prepared::create_fixed(
            handoff,
            command,
            interactive,
            &image,
            &self.delivery.mounts(),
        )?;
        if self.session.delivery()? != self.delivery {
            return Err(Error::Delivery);
        }
        Ok(prepared.spawn()?)
    }
}
