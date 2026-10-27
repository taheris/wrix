//! Explicit host-user controller maintenance, never ordinary shell admission.

use std::{fs, io, os::unix::fs::MetadataExt, time::Duration};

use super::{Client, Error, Operation, Reply, VERSION, recover_registry, supervision};

pub fn repair(host: &wrix_state::host::Host) -> Result<(), Error> {
    let root = host.roots().state().join("control");
    if !root.join("package.json").try_exists()? {
        let _pools = crate::nix_store::lifecycle::unmanaged_repair(host)?;
        return recover_registry(host);
    }
    let package = supervision::Package::from_env().map_err(io::Error::other)?;
    let pools = crate::nix_store::lifecycle::Repair::begin(host, &package)?;
    let native = supervision::Native::for_package(
        supervision::Platform::current().map_err(io::Error::other)?,
        fs::metadata(&root)?.uid(),
        &package,
    );
    native
        .prepare_upgrade(&root, &package)
        .map_err(io::Error::other)?;
    let lifecycle = supervision::Lifecycle::lock(&root).map_err(io::Error::other)?;
    lifecycle
        .upgrade(&native, &package, || {
            recover_registry(host).map_err(io::Error::other)
        })
        .map_err(io::Error::other)?;
    ready(host)?;
    pools.complete()?;
    Ok(())
}

pub fn ready(host: &wrix_state::host::Host) -> Result<(), Error> {
    let root = host.roots().state().join("control");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match Client::open(&root.join("authority")) {
                    Ok(client) => match client.request(Operation::Inspect {}).await {
                        Ok(Reply::Snapshot {
                            version: VERSION, ..
                        }) => return Ok(()),
                        Ok(_) => return Err(Error::ControllerReadiness),
                        Err(Error::Io(error))
                            if matches!(
                                error.kind(),
                                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                            ) =>
                        {
                            tracing::debug!(%error, "awaiting upgraded controller readiness");
                        }
                        Err(error) => return Err(error),
                    },
                    Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                        tracing::debug!(%error, "awaiting retained controller authority");
                    }
                    Err(error) => return Err(error),
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| Error::ControllerReadiness)?
    })?;
    Ok(())
}
