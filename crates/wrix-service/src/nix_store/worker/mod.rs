//! Package-selected host preparation, independent of Celler permission and worker runtime policy.
use crate::control::{
    Client, Operation, PoolPreparation as Status, Reply, VERSION,
    supervision::{Lifecycle, Native, Package, Platform},
};
use std::{io, time::Duration};
use wrix_core::nix_store::{binding::Binding, pool::System};
use wrix_sandbox::nix_store::{Delivery, Error, Factory, Session};
use wrix_state::{
    context::Selection, execution::Ticket, host::Host as Capability,
    nix_store::admission::request::Plane,
};

pub struct Host;
/// SDK attachment to an already-owned authenticated controller; it never starts or replaces supervision.
pub struct Connected {
    client: Client,
    runtime: tokio::runtime::Runtime,
}
impl Connected {
    pub fn new(client: Client) -> Result<Self, Error> {
        Ok(Self {
            client,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
        })
    }
}
struct Source {
    delivery: super::owner::backend::client::Delivery,
}
impl Session for Source {
    fn apple(&self) -> Result<std::process::Command, Error> {
        Ok(self.delivery.apple().map_err(io::Error::other)?)
    }
    fn start_apple_bridge(
        &self,
        worker: &wrix_sandbox::execution::apple::Worker,
    ) -> Result<Box<dyn wrix_sandbox::nix_store::Transport>, Error> {
        Ok(Box::new(
            self.delivery
                .start_apple_bridge(worker)
                .map_err(io::Error::other)?,
        ))
    }
    fn krun_runtime(&self) -> Result<Option<std::path::PathBuf>, Error> {
        self.delivery
            .krun_runtime()
            .map_err(|error| Error::Io(io::Error::other(error)))
    }
    fn podman(&self) -> Result<std::process::Command, Error> {
        Ok(self.delivery.podman().map_err(io::Error::other)?)
    }
    fn delivery(&self) -> Result<Delivery, Error> {
        let (directory, delivery) = if self.delivery.plane() == Plane::Apple {
            (
                self.delivery
                    .apple_configuration()
                    .map_err(io::Error::other)?,
                Delivery::apple(
                    self.delivery.pool().clone(),
                    self.delivery.ticket().clone(),
                    self.delivery.port(),
                )?,
            )
        } else {
            (
                self.delivery
                    .podman_configuration()
                    .map_err(io::Error::other)?,
                Delivery::podman(
                    self.delivery.pool().clone(),
                    self.delivery.ticket().clone(),
                    self.delivery.port(),
                )?,
            )
        };
        let mounts = delivery.mounts();
        if mounts[0].source != self.delivery.store() || mounts[1].source != directory {
            return Err(Error::Delivery);
        }
        Ok(delivery)
    }
}
impl Factory for Connected {
    fn apple_image_commands(
        &self,
    ) -> Result<(std::process::Command, std::process::Command), Error> {
        self.admit_apple()?;
        let host = Capability::discover()?;
        self.client
            .check_native_host(&host)
            .map_err(io::Error::other)?;
        let package = Package::load(&host.roots().state().join("control/package.json"))
            .map_err(io::Error::other)?;
        Ok(super::owner::backend::tools::Tools::for_package(&package)
            .and_then(|tools| tools.apple_image_commands())
            .map_err(io::Error::other)?)
    }
    fn admit_apple(&self) -> Result<(), Error> {
        worker_host(Plane::Apple)?;
        let host = Capability::discover()?;
        self.client
            .check_native_host(&host)
            .map_err(io::Error::other)?;
        match self
            .runtime
            .block_on(self.client.request(Operation::InspectNixAppleWorker {}))
            .map_err(io::Error::other)?
        {
            Reply::PoolInterface { version: 1 } => Ok(()),
            _ => Err(Error::Unavailable),
        }
    }
    fn prepare_apple(
        &self,
        selection: &Selection,
        system: System,
        ticket: &Ticket,
        image: &Binding,
    ) -> Result<Box<dyn Session>, Error> {
        self.admit_apple()?;
        let pool = wrix_state::nix_store::admission::Guard::acquire(selection, system)?
            .pool()
            .clone();
        self.runtime.block_on(prepare(
            &self.client,
            selection,
            &pool,
            ticket,
            image,
            Plane::Apple,
        ))
    }
    fn admit(&self) -> Result<(), Error> {
        podman_host()
    }
    fn admit_krun(&self) -> Result<(), Error> {
        podman_host()
    }
    fn prepare_podman(
        &self,
        selection: &Selection,
        system: System,
        ticket: &Ticket,
        image: &Binding,
    ) -> Result<Box<dyn Session>, Error> {
        self.admit()?;
        let pool = wrix_state::nix_store::admission::Guard::acquire(selection, system)?
            .pool()
            .clone();
        self.runtime.block_on(prepare(
            &self.client,
            selection,
            &pool,
            ticket,
            image,
            Plane::Podman,
        ))
    }
    fn prepare_krun(
        &self,
        selection: &Selection,
        system: System,
        ticket: &Ticket,
        image: &Binding,
    ) -> Result<Box<dyn Session>, Error> {
        self.admit_krun()?;
        let pool = wrix_state::nix_store::admission::Guard::acquire(selection, system)?
            .pool()
            .clone();
        self.runtime.block_on(prepare(
            &self.client,
            selection,
            &pool,
            ticket,
            image,
            Plane::Krun,
        ))
    }
}
impl Factory for Host {
    fn apple_image_commands(
        &self,
    ) -> Result<(std::process::Command, std::process::Command), Error> {
        self.admit_apple()?;
        Ok(super::owner::backend::tools::Tools::from_env()
            .and_then(|tools| tools.apple_image_commands())
            .map_err(io::Error::other)?)
    }
    fn admit_apple(&self) -> Result<(), Error> {
        Self::admit_on(Plane::Apple)
    }
    fn prepare_apple(
        &self,
        selection: &Selection,
        system: System,
        ticket: &Ticket,
        image: &Binding,
    ) -> Result<Box<dyn Session>, Error> {
        Self::prepare_on(selection, system, ticket, image, Plane::Apple)
    }
    fn admit(&self) -> Result<(), Error> {
        Self::admit_on(Plane::Podman)
    }
    fn admit_krun(&self) -> Result<(), Error> {
        Self::admit_on(Plane::Krun)
    }
    fn prepare_podman(
        &self,
        selection: &Selection,
        system: System,
        ticket: &Ticket,
        image: &Binding,
    ) -> Result<Box<dyn Session>, Error> {
        Self::prepare_on(selection, system, ticket, image, Plane::Podman)
    }
    fn prepare_krun(
        &self,
        selection: &Selection,
        system: System,
        ticket: &Ticket,
        image: &Binding,
    ) -> Result<Box<dyn Session>, Error> {
        Self::prepare_on(selection, system, ticket, image, Plane::Krun)
    }
}
impl Host {
    fn admit_on(plane: Plane) -> Result<(), Error> {
        worker_host(plane)?;
        Package::from_env().map_err(io::Error::other)?;
        super::image::Candidate::from_env().map_err(io::Error::other)?;
        super::owner::backend::tools::Tools::from_env()
            .and_then(|tools| tools.view_check(plane))
            .map_err(io::Error::other)?;
        let host = Capability::discover()?;
        super::lifecycle::gate::check_clear(&host).map_err(io::Error::other)?;
        Ok(())
    }
    fn prepare_on(
        selection: &Selection,
        system: System,
        ticket: &Ticket,
        image: &Binding,
        plane: Plane,
    ) -> Result<Box<dyn Session>, Error> {
        Self::admit_on(plane)?;
        // Approval precedes controller effects; no host writer is held across supervision or native I/O.
        let pool = wrix_state::nix_store::admission::Guard::acquire(selection, system)?
            .pool()
            .clone();
        let host = Capability::discover()?;
        let _admission = super::lifecycle::gate::shared(&host).map_err(io::Error::other)?;
        let root = host.roots().state().join("control");
        let package = Package::from_env().map_err(io::Error::other)?;
        selection.authorize_effects()?;
        {
            let lifecycle = Lifecycle::lock(&root).map_err(io::Error::other)?;
            lifecycle
                .start(
                    &Native::for_package(
                        Platform::current().map_err(io::Error::other)?,
                        rustix::process::geteuid().as_raw(),
                        &package,
                    ),
                    &package,
                )
                .map_err(io::Error::other)?;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let client = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    match Client::open(&root.join("authority")) {
                        Ok(client) => match client.request(Operation::Inspect {}).await {
                            Ok(Reply::Snapshot {
                                version: VERSION, ..
                            }) => return Ok(client),
                            Ok(_) => return Err(Error::Unavailable),
                            Err(crate::control::Error::Io(error))
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                                ) =>
                            {
                                tracing::debug!(%error, "awaiting owned pool controller readiness");
                            }
                            Err(error) => return Err(Error::Io(io::Error::other(error))),
                        },
                        Err(crate::control::Error::Io(error))
                            if error.kind() == io::ErrorKind::NotFound =>
                        {
                            tracing::debug!(%error, "awaiting owned pool controller authority");
                        }
                        Err(error) => return Err(Error::Io(io::Error::other(error))),
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .map_err(|_| Error::Unavailable)??;
            prepare(&client, selection, &pool, ticket, image, plane).await
        })
    }
}
fn worker_host(plane: Plane) -> Result<(), Error> {
    let supported = match plane {
        Plane::Apple => cfg!(all(target_os = "macos", target_arch = "aarch64")),
        Plane::Podman | Plane::Krun => cfg!(target_os = "linux"),
        Plane::Host => false,
    };
    if !supported {
        return Err(Error::Unavailable);
    }
    Capability::discover()?;
    Ok(())
}
fn podman_host() -> Result<(), Error> {
    worker_host(Plane::Podman)
}
async fn prepare(
    client: &Client,
    selection: &Selection,
    pool: &wrix_core::nix_store::pool::Key,
    ticket: &Ticket,
    image: &Binding,
    plane: Plane,
) -> Result<Box<dyn Session>, Error> {
    // The checked API probes before publishing a one-use intent and retains the original snapshot.
    let id = match plane {
        Plane::Podman => {
            client
                .prepare_nix_podman_worker(selection, pool.system, ticket, image)
                .await
        }
        Plane::Krun => {
            client
                .prepare_nix_krun_worker(selection, pool.system, ticket, image)
                .await
        }
        Plane::Apple => {
            client
                .prepare_nix_apple_worker(selection, pool.system, ticket, image)
                .await
        }
        Plane::Host => return Err(Error::Delivery),
    }
    .map_err(io::Error::other)?;
    let delivery = tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            match client
                .request(Operation::InspectNixExecutionDelivery { id: id.clone() })
                .await
                .map_err(io::Error::other)?
            {
                Reply::PoolDelivery { delivery } => return Ok(delivery),
                Reply::PoolPreparation {
                    status: Status::Pending,
                } => tokio::time::sleep(Duration::from_millis(50)).await,
                _ => return Err(Error::Unavailable),
            }
        }
    })
    .await
    .map_err(|_| Error::Unavailable)??;
    let source = Source { delivery };
    if source.delivery.ticket() != ticket
        || source.delivery.pool() != pool
        || source
            .delivery
            .krun_runtime()
            .map_err(io::Error::other)?
            .is_some()
            != (plane == Plane::Krun)
    {
        return Err(Error::Delivery);
    }
    source.delivery()?;
    Ok(Box::new(source))
}
