//! Explicit stopped-update endpoint recovery; never retarget released native ownership.

use super::{Intent, Phase};
use crate::{
    control::{
        self,
        supervision::{Lifecycle, Package},
    },
    lifecycle::{Error, Result, Runtime, RuntimeStatus, owner},
};
use std::io;
use wrix_sandbox::image::Digest;
use wrix_state::host::Host;

pub(super) fn repair_endpoints(
    host: &Host,
    intent: &Intent,
    selected: &owner::Selected,
    runtime: &Runtime,
    status: RuntimeStatus,
    digest: &Digest,
) -> Result<()> {
    if !control::endpoints_need_repair(host.roots().state()).map_err(io::Error::other)? {
        return Ok(());
    }
    let root = host.roots().state().join("host-lifecycle");
    let shutdown = super::super::Shutdown::read(&root)?.ok_or_else(refused)?;
    if shutdown.phase != super::super::Phase::Stopped
        || crate::lifecycle::generation::provisioned(host.roots().state())?
        || owner::Selected::read(&root.join("celler-candidate"))?.is_some()
    {
        return Err(refused());
    }
    shutdown.check(host, selected, &shutdown.package)?;
    if !super::super::ownership::recorded_native_owners(&shutdown.locks)
        || crate::dolt::server::socket::ready(&host.roots().state().join("service-data/dolt"))
    {
        return Err(refused());
    }
    if runtime.status(&selected.container)? != status {
        return Err(refused());
    }
    let recipe = selected.recipe.as_ref().ok_or_else(refused)?;
    match (&intent.ownership, intent.phase, status) {
        (None, Phase::Prepared | Phase::Preparing, RuntimeStatus::Stopped) => (),
        (Some(ownership), Phase::Starting, RuntimeStatus::Missing) => {
            let origin = ownership.origin.as_ref().ok_or_else(refused)?;
            if ownership.locks != shutdown.locks || recipe.image_id != *origin {
                return Err(refused());
            }
            super::super::checkpoint::verify(
                host.roots().state(),
                &intent.attempt,
                &intent.candidate,
                digest,
                Some(origin),
            )?;
        }
        _ => return Err(refused()),
    }
    let layout = wrix_cache::backend::layout::Layout::current()?;
    let pins = super::super::ownership::Pin::capture(host.roots().state(), layout.metadata())?;
    if pins
        .iter()
        .map(super::super::ownership::Pin::record)
        .collect::<Vec<_>>()
        != shutdown.locks
    {
        return Err(refused());
    }
    for pin in &pins {
        pin.confirm()?;
    }
    let control_root = host.roots().state().join("control");
    let controller = Lifecycle::lock(&control_root).map_err(io::Error::other)?;
    let retained = Package::load(&control_root.join("package.json")).map_err(io::Error::other)?;
    if retained != shutdown.package && retained != intent.controller {
        return Err(refused());
    }
    let native = super::super::native(host, &retained)?;
    if controller
        .preflight(&native, &retained)
        .map_err(io::Error::other)?
        .as_ref()
        != Some(&retained)
    {
        return Err(refused());
    }
    controller.stop_owned(&native).map_err(io::Error::other)?;
    control::repair_retained_endpoints(host.roots().state(), intent.attempt.as_str())
        .map_err(io::Error::other)?;
    for pin in &pins {
        pin.confirm()?;
    }
    Ok(())
}

fn refused() -> Error {
    Error::Operation { message: "retained role endpoints conflict with controller leases; automatic relocation is forbidden. Current Wrix `service update --resume` can repair only a verified explicitly stopped legacy transition before release or a checkpointed Starting transition with no backend; no native generations may be provisioned; ownership, data and update intent remain retained".into() }
}
