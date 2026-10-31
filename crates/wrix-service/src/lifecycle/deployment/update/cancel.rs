//! Cancel only before native maintenance; retain the exact intent as durable evidence.

use super::{FILE, Intent};
use crate::lifecycle::{Error, Result, RuntimeStatus, coordination, owner};
use std::{fs, io, path::Path};
use wrix_state::host::Host;

pub fn run() -> Result<String> {
    let host = Host::discover().map_err(io::Error::other)?;
    let deployment = crate::control::existing_deployment(&host)
        .map_err(io::Error::other)?
        .ok_or_else(|| Error::Operation {
            message: "update cancellation requires an existing deployment".into(),
        })?;
    let _update = coordination::acquire(&host.roots().state().join("service-update"))?;
    let _pools = crate::nix_store::lifecycle::gate::exclusive(&host).map_err(io::Error::other)?;
    crate::nix_store::lifecycle::gate::check_clear(&host).map_err(io::Error::other)?;
    let root = host.roots().state().join("host-lifecycle");
    let guard = coordination::acquire(&root)?;
    let Some(bytes) = super::document(guard.root(), FILE)? else {
        return Ok("No pending service update to cancel.\n".into());
    };
    let intent: Intent = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    intent.check_cancellable()?;
    check_checkpoint(&root, &intent)?;
    if intent.deployment != deployment || super::super::Shutdown::read(guard.root())?.is_some() {
        return Err(owner::Error::Conflict.into());
    }
    if owner::Selected::read(&root.join("celler-candidate"))?.is_some() {
        return Err(owner::Error::Conflict.into());
    }
    let selected = owner::Selected::read(&root)?.ok_or(owner::Error::Invalid)?;
    if selected.recipe.is_none() {
        return Err(Error::Operation {
            message:
                "selected backend lacks immutable image provenance; update cancellation refused"
                    .into(),
        });
    }
    let runtime = super::super::runtime(&selected)?;
    super::super::validate_inventory(&runtime, &selected)?;
    if runtime.status(&selected.container)? != RuntimeStatus::Running {
        return Err(Error::Operation {
            message:
                "cannot prove the original service is still running; update cancellation refused"
                    .into(),
        });
    }
    let control = host.roots().state().join("control");
    if super::document(&control, "stopped")?.is_some() {
        return Err(owner::Error::Conflict.into());
    }
    let controller =
        crate::control::supervision::Lifecycle::lock(&control).map_err(io::Error::other)?;
    let package = super::super::retained_package(&host)?;
    let native = super::super::native(&host, &package)?;
    if controller
        .preflight(&native, &package)
        .map_err(io::Error::other)?
        .as_ref()
        != Some(&package)
    {
        return Err(owner::Error::Conflict.into());
    }
    if intent.version == 1 || intent.legacy_refusal() {
        check_legacy(&host, &intent, &selected, &package)?;
    }
    crate::control::maintenance::ready(&host).map_err(io::Error::other)?;
    let archive = archive(guard.root(), &bytes, &intent)?;
    Ok(format!(
        "Cancelled unstarted service update. Running services, data, signing state and software roots unchanged. Retained intent: {}\n",
        archive.display()
    ))
}

fn check_checkpoint(root: &Path, intent: &Intent) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let checkpoints = root.join("checkpoints");
    match fs::symlink_metadata(&checkpoints) {
        Ok(metadata)
            if !metadata.is_dir()
                || metadata.mode() & 0o077 != 0
                || metadata.uid() != fs::metadata(root)?.uid() =>
        {
            return Err(owner::Error::Permissions.into());
        }
        Ok(_) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    match fs::symlink_metadata(checkpoints.join(intent.attempt.as_str())) {
        Ok(_) => Err(Error::Operation { message: "native checkpoint exists for this update; cancellation refused; retain its intent and checkpoint for coordinated recovery".into() }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn check_legacy(
    host: &Host,
    intent: &Intent,
    selected: &owner::Selected,
    controller: &crate::control::supervision::Package,
) -> Result<()> {
    if crate::lifecycle::generation::provisioned(host.roots().state())?
        || crate::nix_store::lifecycle::needs_binding(host).map_err(io::Error::other)?
        || selected
            .recipe
            .as_ref()
            .is_none_or(|recipe| recipe.binding == intent.candidate)
        || *controller == intent.controller
    {
        return Err(Error::Operation {
            message: "legacy update lacks proof of an unstarted transition; cancellation refused. Retain its exact package intent and use coordinated maintenance".into(),
        });
    }
    Ok(())
}

pub(super) fn archive(root: &Path, bytes: &[u8], intent: &Intent) -> Result<std::path::PathBuf> {
    use std::io::Write;
    intent.check_cancellable()?;
    let name = format!("cancelled-update-{}.json", intent.attempt.as_str());
    match super::document(root, &name)? {
        Some(retained) if retained == bytes => (),
        Some(_) => return Err(owner::Error::Conflict.into()),
        None => {
            let mut file = tempfile::NamedTempFile::new_in(root)?;
            file.write_all(bytes)?;
            file.as_file().sync_all()?;
            file.persist_noclobber(root.join(&name))
                .map_err(|error| error.error)?;
        }
    }
    fs::File::open(root)?.sync_all()?;
    if super::document(root, FILE)?.as_deref() != Some(bytes) {
        return Err(owner::Error::Conflict.into());
    }
    fs::remove_file(root.join(FILE))?;
    fs::File::open(root)?.sync_all()?;
    Ok(root.join(name))
}
