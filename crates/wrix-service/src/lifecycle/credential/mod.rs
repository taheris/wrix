//! Host admission and current-policy renewal, outside worker/service authority.

use std::{io, path::Path, time::Duration};

use tokio::{
    net::UnixStream,
    runtime::{Builder, Runtime},
    time::{Instant, timeout},
};
use wrix_sandbox::cache_credentials::{Error, Factory, Grant, Issuer};
use wrix_state::{context::Selection, host::Host as Capability};

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
    selection: Selection,
}

impl Factory for Host {
    fn admit(&self) -> Result<(), Error> {
        Capability::discover().map_err(|_| Error::Unavailable)?;
        Package::from_env()
            .map(|_| ())
            .map_err(|_| Error::Unavailable)
    }

    fn open(&self, selection: &Selection) -> Result<Box<dyn Issuer>, Error> {
        open(selection).map(|source| Box::new(source) as Box<dyn Issuer>).map_err(|error| {
            tracing::error!(%error, workspace = %selection.workspace().canonical_path().display(), "host cache credential admission failed");
            Error::Unavailable
        })
    }
}

#[derive(Debug, displaydoc::Display, thiserror::Error)]
enum OpenError {
    /// {0}
    Io(#[from] io::Error),
    /// {0}
    State(#[from] wrix_state::store::Error),
    /// {0}
    Capability(#[from] wrix_state::host::Error),
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

fn open(selection: &Selection) -> Result<Source, OpenError> {
    Capability::discover()?;
    let package = Package::from_env()?;
    selection.authorize_effects()?;
    let workspace = selection.workspace();
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
    let uid = runtime.block_on(async {
        let (local, _peer) = UnixStream::pair()?;
        Ok::<_, io::Error>(local.peer_cred()?.uid())
    })?;
    {
        let lifecycle = Lifecycle::lock(&root)?;
        selection.recheck()?;
        lifecycle.start(
            &Native::for_package(Platform::current()?, uid, &package),
            &package,
        )?;
    }
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
            Ok(client)
        })
        .await
        .map_err(|_| OpenError::Deadline)?
    })?;
    let Reply::Snapshot {
        version: VERSION,
        namespaces,
    } = runtime.block_on(client.request(Operation::Inspect {}))?
    else {
        return Err(OpenError::Admission);
    };
    if !namespaces.contains_key(&namespace) {
        // Resolver rechecks are synchronous and own their bounded runtime.
        selection.recheck()?;
        let policy = Policy {
            dolt: Access::Denied,
            private_cache_read: Access::Allowed,
            common_cache_read: Access::Allowed,
            egress: Access::Denied,
            compiler: Access::Denied,
        };
        if runtime.block_on(client.request(Operation::Register {
            namespace: namespace.clone(),
            policy,
        }))? != Reply::Applied
        {
            return Err(OpenError::Admission);
        }
    }
    Ok(Source {
        runtime,
        client,
        namespace,
        selection: selection.clone(),
    })
}

impl Issuer for Source {
    fn issue(&mut self) -> Result<Grant, Error> {
        self.selection.recheck().map_err(|_| Error::Unavailable)?;
        self.runtime
            .block_on(self.client.cache_credential(self.namespace.clone()))
            .map_err(|_| Error::Unavailable)
    }
}
