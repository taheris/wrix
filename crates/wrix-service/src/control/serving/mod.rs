//! Controller-owned serving lifetime for private control and the shared data front door.

pub mod apple;
mod native;

use std::{future::Future, net::SocketAddr, num::NonZeroUsize};

use super::{Controller, Error};
use crate::route::{FrontDoor, Role, Selection};

#[cfg(target_os = "macos")]
fn guest(controller: &Controller, front_door: FrontDoor) -> Result<FrontDoor, Error> {
    let root = controller.authority_root()?;
    let control = root.parent().ok_or(Error::PrivateRoot)?;
    let package = super::supervision::Package::load(&control.join("package.json"))
        .map_err(std::io::Error::other)?;
    let tools = crate::nix_store::owner::backend::tools::Tools::for_package(&package)
        .map_err(std::io::Error::other)?;
    let observation = apple::discover(&tools)?;
    let ports = controller
        .endpoints
        .bound_ports()?
        .ok_or(Error::InvalidEndpoints)?;
    let listeners = observation.bind(ports)?;
    if apple::discover(&tools)? != observation {
        return Err(Error::InvalidEndpoints);
    }
    controller
        .endpoints
        .publish_guest(observation.address().gateway())?;
    Ok(front_door.with_guest(listeners, observation.address()))
}

/// One controller retains all role listeners; registration cannot replace or extend them.
/// Listening does not establish native readiness, and every route starts closed.
pub struct Host {
    controller: Controller,
    front_door: FrontDoor,
}

impl Host {
    pub async fn bind(controller: Controller, capacity: NonZeroUsize) -> Result<Self, Error> {
        let front_door = async {
            let listeners = controller.bind_role_listeners().await?;
            let front_door = FrontDoor::new(listeners, capacity)?;
            #[cfg(target_os = "macos")]
            let front_door = guest(&controller, front_door)?;
            Ok::<_, Error>(front_door)
        }
        .await;
        match front_door {
            Ok(front_door) => Ok(Self {
                controller,
                front_door,
            }),
            Err(error) => {
                if let Err(cleanup) = controller.close() {
                    tracing::error!(%cleanup, %error, "failed to release unstarted controller endpoint");
                }
                Err(error)
            }
        }
    }

    /// Host-only managers must establish native readiness before selecting any route.
    pub fn selection(&self) -> Selection {
        self.front_door.selection()
    }

    pub fn address(&self, role: Role) -> Result<SocketAddr, Error> {
        Ok(self.front_door.address(role)?)
    }

    /// Explicit host shutdown is disruptive, not a generation drain or ownership release.
    /// Invalid client requests do not stop peer control or data service.
    pub async fn serve(self, shutdown: impl Future<Output = ()> + Send) -> Result<(), Error> {
        let Self {
            controller,
            front_door,
        } = self;
        let selection = front_door.selection();
        let forwarding = front_door.serve(std::future::pending());
        tokio::pin!(forwarding, shutdown);
        let result = {
            let attachment = native::run(&controller, selection);
            tokio::pin!(attachment);
            loop {
                tokio::select! {
                    () = &mut shutdown => break Ok(()),
                    result = &mut forwarding => break result.map_err(Error::from),
                    result = &mut attachment => break result,
                    request = controller.serve_one() => {
                        if let Err(error) = request {
                            tracing::warn!(%error, "private controller request failed");
                        }
                    }
                }
            }
        };
        if let Some(issuer) = controller.gateway.read().await.as_ref() {
            issuer.revoke_all().await;
        }
        controller.close()?;
        result
    }
}
