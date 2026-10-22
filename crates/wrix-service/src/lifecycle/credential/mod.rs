//! Host admission and current-policy renewal, outside worker/service authority.

use std::{io, path::Path, time::Duration};

use tokio::{
    net::UnixStream,
    runtime::{Builder, Runtime},
    time::{Instant, timeout},
};
use wrix_core::path::Workspace;
use wrix_sandbox::cache_credentials::{Error, Factory, Grant, Issuer};

use super::Paths;
use crate::control::{
    Access, Client, Namespace, Operation, Policy, Reply, VERSION,
    supervision::{Lifecycle, Native, Package, Platform},
};

pub struct Host;

struct Source {
    runtime: Runtime,
    client: Client,
    namespace: Namespace,
}

impl Factory for Host {
    fn admit(&self) -> Result<(), Error> {
        Package::from_env()
            .map(|_| ())
            .map_err(|_| Error::Unavailable)
    }

    fn open(&self, workspace: &Path) -> Result<Box<dyn Issuer>, Error> {
        open(workspace).map(|source| Box::new(source) as Box<dyn Issuer>).map_err(|error| {
            tracing::error!(%error, workspace = %workspace.display(), "host cache credential admission failed");
            Error::Unavailable
        })
    }
}

#[derive(Debug, displaydoc::Display, thiserror::Error)]
enum OpenError {
    /// {0}
    Io(#[from] io::Error),
    /// {0}
    Lifecycle(#[from] super::Error),
    /// {0}
    Supervision(#[from] crate::control::supervision::Error),
    /// {0}
    Control(#[from] crate::control::Error),
    /// {0}
    Publication(#[from] wrix_cache::backend::publication::Error),
    /// host controller did not confirm compatible namespace admission
    Admission,
    /// host controller readiness exceeded its deadline
    Deadline,
}

fn open(path: &Path) -> Result<Source, OpenError> {
    let package = Package::from_env()?;
    let workspace = Workspace::from_service_path(path)?;
    let namespace = Namespace::from(workspace.hash());
    let paths = Paths::for_workspace(workspace.hash())?;
    // Serialize initial admission with start/stop. Passive loading rejects a
    // retirement committed after service startup; renewal never takes this
    // path or recreates a retired namespace.
    let _registration = paths.lock_registration()?;
    let _publication =
        wrix_cache::backend::publication::Publisher::load(paths.state_root(), workspace.hash())?;
    let root = paths
        .state_root()
        .parent()
        .and_then(Path::parent)
        .ok_or(OpenError::Admission)?
        .join("control");
    wrix_core::private_directory::prepare_with_parents(&root).map_err(io::Error::other)?;
    let runtime = Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let (local, peer) = UnixStream::pair()?;
        let uid = local.peer_cred()?.uid();
        drop((local, peer));
        let lifecycle = Lifecycle::lock(&root)?;
        lifecycle.start(
            &Native::for_package(Platform::current()?, uid, &package),
            &package,
        )?;
        Ok::<(), OpenError>(())
    })?;
    let client = runtime.block_on(async {
        timeout(Duration::from_secs(15), async {
            let deadline = Instant::now() + Duration::from_secs(15);
            let client = loop {
                match Client::open(&root.join("authority")) {
                    Ok(client) => match client.request(Operation::Inspect {}).await {
                        Ok(Reply::Snapshot {
                            version: VERSION, ..
                        }) => break client,
                        Ok(_) => return Err(OpenError::Admission),
                        Err(crate::control::Error::Io(error))
                            if matches!(
                                error.kind(),
                                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                            ) =>
                        {
                            tracing::debug!(%error, "awaiting owned host controller readiness");
                        }
                        Err(error) => return Err(error.into()),
                    },
                    Err(crate::control::Error::Io(error))
                        if error.kind() == io::ErrorKind::NotFound =>
                    {
                        tracing::debug!(%error, "awaiting owned host controller authority");
                    }
                    Err(error) => return Err(error.into()),
                }
                if Instant::now() >= deadline {
                    return Err(OpenError::Deadline);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            let Reply::Snapshot {
                version: VERSION,
                namespaces,
            } = client.request(Operation::Inspect {}).await?
            else {
                return Err(OpenError::Admission);
            };
            if !namespaces.contains_key(&namespace) {
                let policy = Policy {
                    dolt: Access::Denied,
                    private_cache_read: Access::Allowed,
                    common_cache_read: Access::Allowed,
                    egress: Access::Denied,
                    compiler: Access::Denied,
                };
                if client
                    .request(Operation::Register {
                        namespace: namespace.clone(),
                        policy,
                    })
                    .await?
                    != Reply::Applied
                {
                    return Err(OpenError::Admission);
                }
            }
            Ok(client)
        })
        .await
        .map_err(|_| OpenError::Deadline)?
    })?;
    Ok(Source {
        runtime,
        client,
        namespace,
    })
}

impl Issuer for Source {
    fn issue(&mut self) -> Result<Grant, Error> {
        self.runtime
            .block_on(self.client.cache_credential(self.namespace.clone()))
            .map_err(|_| Error::Unavailable)
    }
}
