//! Trusted host factories bind Linux worker delivery and runtime, never worker-selected endpoints or authority.
#[cfg(test)]
mod test;

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
    apple: bool,
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
            apple: false,
        })
    }
    pub fn apple(pool: Key, ticket: Ticket, port: NonZeroU16) -> Result<Self, Error> {
        let mut delivery = Self::podman(pool, ticket, port)?;
        delivery.apple = true;
        Ok(delivery)
    }
    pub fn mounts(&self) -> Vec<crate::execution::ProjectionMount> {
        let mut mounts = vec![
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
            crate::execution::ProjectionMount {
                source: self.configuration.join("state"),
                destination: "/nix/var/nix".into(),
                writable: false,
            },
        ];
        if self.apple {
            mounts.retain(|mount| mount.destination != std::path::Path::new("/etc/nix/nix.conf"));
        }
        mounts
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
        if !self.apple {
            command.arg("-e").arg(format!(
                "WRIX_NETWORK_LOCAL_ENDPOINTS={}",
                self.local_endpoint()
            ));
        }
    }
}
/// Re-observe native export identity and fixed configuration, not historical job readiness.
pub trait Transport {}
pub trait Session {
    fn apple(&self) -> Result<std::process::Command, Error> {
        Err(Error::Unavailable)
    }
    fn start_apple_bridge(
        &self,
        _: &crate::execution::apple::Worker,
    ) -> Result<Box<dyn Transport>, Error> {
        Err(Error::Unavailable)
    }
    fn delivery(&self) -> Result<Delivery, Error>;
    fn podman(&self) -> Result<std::process::Command, Error>;
    fn krun_runtime(&self) -> Result<Option<PathBuf>, Error> {
        Ok(None)
    }
}
pub trait Factory {
    fn apple_image_commands(
        &self,
    ) -> Result<(std::process::Command, std::process::Command), Error> {
        Err(Error::Unavailable)
    }
    fn admit_apple(&self) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
    fn prepare_apple(
        &self,
        _: &Selection,
        _: System,
        _: &Ticket,
        _: &Binding,
    ) -> Result<Box<dyn Session>, Error> {
        Err(Error::Unavailable)
    }
    fn admit(&self) -> Result<(), Error>;
    fn admit_krun(&self) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
    fn prepare_podman(
        &self,
        selection: &Selection,
        system: System,
        ticket: &Ticket,
        image: &Binding,
    ) -> Result<Box<dyn Session>, Error>;
    fn prepare_krun(
        &self,
        _: &Selection,
        _: System,
        _: &Ticket,
        _: &Binding,
    ) -> Result<Box<dyn Session>, Error> {
        Err(Error::Unavailable)
    }
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
        self.prepare_on(factory, Runtime::Podman)
    }
    pub fn prepare_krun(self, factory: &dyn Factory) -> Result<Prepared, Error> {
        self.prepare_on(factory, Runtime::Krun)
    }
    pub fn prepare_apple(self, factory: &dyn Factory) -> Result<Prepared, Error> {
        self.prepare_on(factory, Runtime::Apple)
    }
    fn prepare_on(self, factory: &dyn Factory, runtime: Runtime) -> Result<Prepared, Error> {
        let pool = wrix_state::nix_store::admission::Guard::acquire(&self.selection, self.system)?
            .pool()
            .clone();
        let session = match runtime {
            Runtime::Apple => {
                factory.prepare_apple(&self.selection, self.system, &self.ticket, &self.image)?
            }
            Runtime::Podman => {
                factory.prepare_podman(&self.selection, self.system, &self.ticket, &self.image)?
            }
            Runtime::Krun => {
                factory.prepare_krun(&self.selection, self.system, &self.ticket, &self.image)?
            }
        };
        let delivery = session.delivery()?;
        let krun = session.krun_runtime()?.map(Krun::parse).transpose()?;
        if delivery.pool != pool
            || delivery.ticket != self.ticket
            || !runtime.matches(krun.as_ref())
            || delivery.apple != matches!(runtime, Runtime::Apple)
        {
            return Err(Error::Delivery);
        }
        Ok(Prepared {
            attempt: self,
            session,
            delivery,
            krun,
        })
    }
}
#[derive(Clone, Copy)]
enum Runtime {
    Podman,
    Krun,
    Apple,
}
impl Runtime {
    const fn matches(self, krun: Option<&Krun>) -> bool {
        matches!(
            (self, krun),
            (Self::Podman | Self::Apple, None) | (Self::Krun, Some(_))
        )
    }
}
#[derive(Eq, PartialEq)]
struct Krun(PathBuf);
impl Krun {
    fn parse(path: PathBuf) -> Result<Self, Error> {
        let base = path
            .to_str()
            .and_then(|value| value.strip_prefix("/nix/store/"))
            .and_then(|value| value.strip_suffix("/bin/krun"))
            .and_then(|value| value.split_once('-'))
            .ok_or(Error::Delivery)?;
        if base.0.len() != 32
            || !base
                .0
                .bytes()
                .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&byte))
            || base.1.is_empty()
            || !base
                .1
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+-._?=".contains(&byte))
        {
            return Err(Error::Delivery);
        }
        Ok(Self(path))
    }
    fn as_path(&self) -> &std::path::Path {
        &self.0
    }
}
fn check_runtime(
    command: &std::process::Command,
    image: &str,
    expected: Option<&std::path::Path>,
) -> Result<(), Error> {
    let mut args = command.get_args();
    if args.next() != Some(std::ffi::OsStr::new("run"))
        || args.next() != Some(std::ffi::OsStr::new("--rm"))
    {
        return Err(Error::Delivery);
    }
    let mut selected = None;
    while let Some(argument) = args.next() {
        if argument == std::ffi::OsStr::new(image) {
            return if selected == expected.map(std::path::Path::as_os_str) {
                Ok(())
            } else {
                Err(Error::Delivery)
            };
        }
        let value = if argument == "--runtime" {
            Some(args.next().ok_or(Error::Delivery)?)
        } else {
            argument
                .as_bytes()
                .strip_prefix(b"--runtime=")
                .map(std::ffi::OsStr::from_bytes)
        };
        if let Some(value) = value {
            if selected.is_some() || value.is_empty() {
                return Err(Error::Delivery);
            }
            selected = Some(value);
        } else {
            match argument.as_bytes() {
                b"-e" | b"--env" | b"-v" | b"--volume" | b"--mount" | b"-w" | b"--workdir"
                | b"--entrypoint" | b"--name" | b"--label" | b"--network" | b"--memory"
                | b"--cpus" | b"--pids-limit" | b"--userns" | b"--user" | b"--cap-add" => {
                    args.next().ok_or(Error::Delivery)?;
                }
                b"-i" | b"-t" | b"-it" | b"--interactive" | b"--tty" | b"--init" => (),
                value if value.starts_with(b"--") && value.contains(&b'=') => (),
                _ => return Err(Error::Delivery),
            }
        }
    }
    Err(Error::Delivery)
}

pub struct Prepared {
    attempt: Attempt,
    session: Box<dyn Session>,
    delivery: Delivery,
    krun: Option<Krun>,
}
impl Prepared {
    pub const fn delivery(&self) -> &Delivery {
        &self.delivery
    }
    pub fn command(&self) -> Result<std::process::Command, Error> {
        if self.delivery.apple {
            self.session.apple()
        } else {
            self.session.podman()
        }
    }
    pub fn krun_runtime(&self) -> Option<&std::path::Path> {
        self.krun.as_ref().map(Krun::as_path)
    }
    fn recheck_delivery(&self) -> Result<(), Error> {
        if self.session.delivery()? != self.delivery
            || self.session.krun_runtime()?.map(Krun::parse).transpose()? != self.krun
        {
            return Err(Error::Delivery);
        }
        Ok(())
    }
    pub const fn image(&self) -> &Binding {
        &self.attempt.image
    }
    /// Recheck live delivery, then atomically commit original approval and reservation before native creation.
    pub fn spawn(
        self,
        command: &std::process::Command,
        interactive: bool,
    ) -> Result<Running, Error> {
        self.recheck_delivery()?;
        if self.delivery.apple {
            return self.spawn_apple(command, interactive);
        }
        check_runtime(
            command,
            self.attempt.image.digest().as_str(),
            self.krun_runtime(),
        )?;
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
        self.recheck_delivery()?;
        Ok(Running::Podman(prepared.spawn()?))
    }
    fn spawn_apple(
        self,
        command: &std::process::Command,
        interactive: bool,
    ) -> Result<Running, Error> {
        let runtime = self.session.apple()?;
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
        let prepared = crate::execution::apple::Prepared::create(
            handoff,
            &self.attempt.ticket,
            command,
            interactive,
            &image,
            self.attempt.image.metadata().seed_closure.as_path(),
            &self.delivery.mounts(),
        )?;
        self.recheck_delivery()?;
        Ok(Running::Apple(prepared.spawn(self.session.as_ref())?))
    }
}
pub enum Running {
    Podman(crate::execution::Running),
    Apple(crate::execution::apple::Running),
}
impl Running {
    pub const fn child(&mut self) -> &mut std::process::Child {
        match self {
            Self::Podman(value) => value.child(),
            Self::Apple(value) => value.child(),
        }
    }
    pub fn wait_for_retirement(
        &self,
        budget: std::time::Duration,
    ) -> Result<crate::execution::Observation, crate::execution::Error> {
        match self {
            Self::Podman(value) => value.wait_for_retirement(budget),
            Self::Apple(value) => value.wait_for_retirement(budget),
        }
    }
}
