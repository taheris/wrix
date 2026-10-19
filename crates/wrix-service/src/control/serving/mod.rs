//! Controller-owned serving lifetime for private control and the shared data front door.

use std::{future::Future, net::SocketAddr, num::NonZeroUsize};

use super::{Controller, Error};
use crate::route::{FrontDoor, Role, Selection};

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
            Ok::<_, Error>(FrontDoor::new(listeners, capacity)?)
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
        let forwarding = front_door.serve(std::future::pending());
        tokio::pin!(forwarding, shutdown);
        let result = loop {
            tokio::select! {
                () = &mut shutdown => break Ok(()),
                result = &mut forwarding => break result.map_err(Error::from),
                request = controller.serve_one() => {
                    if let Err(error) = request {
                        tracing::warn!(%error, "private controller request failed");
                    }
                }
            }
        };
        controller.close()?;
        result
    }
}
