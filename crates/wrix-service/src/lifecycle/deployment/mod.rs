//! Explicit whole-host shutdown retains selected software and native data; ordinary start only resumes that selection.

mod backend;
mod checkpoint;
mod ownership;
mod update;
pub use update::{cancel as cancel_update, resume as resume_update, run as update};

use super::{Error, Result, Runtime, RuntimeKind, RuntimeStatus, coordination, owner};
use crate::control::supervision::{Lifecycle, Native, Package, Platform};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    process::{Command, Stdio},
};
use wrix_core::path::ContainerName;
use wrix_state::host::Host;

const SHUTDOWN: &str = "shutdown.json";
const LIMIT: u64 = 32 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Stopping,
    Stopped,
    Resuming,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Shutdown {
    version: u16,
    deployment: crate::deployment::Id,
    package: Package,
    runtime: RuntimeKind,
    container: String,
    image: wrix_sandbox::image::Digest,
    locks: Vec<ownership::Record>,
    phase: Phase,
}

impl Shutdown {
    fn read(root: &Path) -> Result<Option<Self>> {
        let Some(bytes) = document(root, SHUTDOWN)? else {
            return Ok(None);
        };
        let record: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if record.version != 1
            || record.locks.len() != 3
            || ContainerName::from_persisted(record.container.clone()).is_none()
        {
            return Err(owner::Error::Invalid.into());
        }
        Ok(Some(record))
    }

    fn publish(&self, root: &Path) -> Result<()> {
        publish(root, SHUTDOWN, self)
    }

    fn check(&self, host: &Host, selected: &owner::Selected, package: &Package) -> Result<()> {
        if crate::control::existing_deployment(host)
            .map_err(io::Error::other)?
            .as_ref()
            != Some(&self.deployment)
            || self.package != *package
            || self.runtime != selected.runtime
            || self.container != selected.container.as_str()
            || selected
                .recipe
                .as_ref()
                .is_some_and(|recipe| recipe.image_id != self.image)
        {
            return Err(owner::Error::Conflict.into());
        }
        if selected.recipe.is_none()
            && runtime(selected)?
                .container_image_id(&selected.container)?
                .as_ref()
                != Some(&self.image)
        {
            return Err(owner::Error::Conflict.into());
        }
        Ok(())
    }
}

pub(crate) fn document(root: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    let file = match File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(name))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != fs::metadata(root)?.uid()
        || metadata.nlink() != 1
        || metadata.len() > LIMIT
    {
        return Err(owner::Error::Permissions.into());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(owner::Error::Invalid.into());
    }
    Ok(Some(bytes))
}

pub(super) fn publish(root: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() as u64 > LIMIT {
        return Err(owner::Error::Invalid.into());
    }
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(root.join(name)).map_err(|error| error.error)?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn retained_package(host: &Host) -> Result<Package> {
    Package::load(&host.roots().state().join("control/package.json")).map_err(|error| {
        Error::Operation {
            message: format!("cannot establish the recorded host controller package: {error}"),
        }
    })
}

fn runtime(selected: &owner::Selected) -> Result<Runtime> {
    let mut runtime = Runtime::from_env()?;
    if runtime.kind != selected.runtime {
        runtime = runtime.alternate();
    }
    runtime.binding = selected
        .recipe
        .as_ref()
        .map(|recipe| recipe.binding.clone());
    Ok(runtime)
}

fn native(host: &Host, package: &Package) -> Result<Native> {
    Ok(Native::for_package(
        Platform::current().map_err(io::Error::other)?,
        fs::metadata(host.roots().state())?.uid(),
        package,
    ))
}

fn recorded_owner(host: &Host) -> Result<owner::Selected> {
    let root = host.roots().state().join("host-lifecycle");
    let selected = owner::Selected::read(&root)?;
    let pending = owner::Selected::read(&root.join("celler-candidate"))?;
    if let (Some(selected), Some(pending)) = (&selected, &pending)
        && (selected.runtime != pending.runtime
            || selected.container != pending.container
            || selected.port != pending.port)
    {
        return Err(owner::Error::Conflict.into());
    }
    pending.or(selected).ok_or(Error::RecoveryScope)
}

fn validate_inventory(runtime: &Runtime, selected: &owner::Selected) -> Result<()> {
    super::shared::check_inventory(runtime, Some(selected), None)?;
    let status = runtime.status(&selected.container)?;
    if status != RuntimeStatus::Missing {
        owner::host::validate(runtime, selected, status)?;
    }
    Ok(())
}

/// This operation never removes a runtime container, persistent data, an image or a retention root.
pub fn stop() -> Result<String> {
    let host = Host::discover().map_err(io::Error::other)?;
    let Some(deployment) = crate::control::existing_deployment(&host).map_err(io::Error::other)?
    else {
        return Ok("scope: host-user\ndeployment: absent\n".into());
    };
    let package = retained_package(&host)?;
    let selected = recorded_owner(&host)?;
    let runtime = runtime(&selected)?;
    validate_inventory(&runtime, &selected)?;
    let image = match &selected.recipe {
        Some(recipe) => recipe.image_id.clone(),
        None => runtime
            .container_image_id(&selected.container)?
            .ok_or(owner::Error::Invalid)?,
    };
    let pools =
        crate::nix_store::lifecycle::Shutdown::begin(&host, &package).map_err(io::Error::other)?;
    let guard = coordination::acquire(&host.roots().state().join("host-lifecycle"))?;
    if recorded_owner(&host)? != selected {
        return Err(owner::Error::Conflict.into());
    }
    let layout = wrix_cache::backend::layout::Layout::current()?;
    let pins = ownership::Pin::capture(host.roots().state(), layout.metadata())?;
    let locks = pins.iter().map(ownership::Pin::record).collect::<Vec<_>>();
    let mut record = match Shutdown::read(guard.root())? {
        Some(mut record) => {
            record.package = package.clone();
            record.image = image;
            record.phase = Phase::Stopping;
            record.check(&host, &selected, &package)?;
            record
        }
        None => Shutdown {
            version: 1,
            deployment,
            package: package.clone(),
            runtime: selected.runtime,
            container: selected.container.as_str().into(),
            image,
            locks: locks.clone(),
            phase: Phase::Stopping,
        },
    };
    record.publish(guard.root())?;
    let controller =
        Lifecycle::lock(&host.roots().state().join("control")).map_err(io::Error::other)?;
    controller
        .stop_owned(&native(&host, &package)?)
        .map_err(io::Error::other)?;
    validate_inventory(&runtime, &selected)?;
    super::generation::stop(&host)?;
    if runtime.status(&selected.container)? == RuntimeStatus::Running {
        let output = Command::new(&runtime.binary)
            .args(["stop", selected.container.as_str()])
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(Error::Operation {
                message: format!(
                    "host shutdown remains incomplete: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
    }
    if runtime.status(&selected.container)? == RuntimeStatus::Running {
        return Err(Error::Operation {
            message:
                "host shutdown did not confirm native backend stop; data and selection retained"
                    .into(),
        });
    }
    for pin in &pins {
        pin.confirm()?;
    }
    if record.locks != locks {
        let authorized = selected
            .recipe
            .as_ref()
            .map(|recipe| allows_update(host.roots().state(), &recipe.binding))
            .transpose()?
            .unwrap_or(false);
        if !authorized || !ownership::extends(&record.locks, &locks) {
            return Err(Error::Operation { message: "traffic and runtimes are stopped, but original storage ownership changed; data and recovery intent retained for explicit reconciliation".into() });
        }
        record.locks = locks;
    }
    record.phase = Phase::Stopped;
    record.publish(guard.root())?;
    pools.complete().map_err(io::Error::other)?;
    drop(pins);
    Ok(format!(
        "scope: host-user\ndeployment: {}\nselected container: {}\nstate: stopped\ntraffic grants: revoked; not restored by restart\ndata and software roots: retained\n",
        record.deployment, selected.container
    ))
}

/// Resume only a completed explicit shutdown, including an interrupted resume of that exact intent.
pub(super) fn resume(host: &Host) -> Result<()> {
    let root = host.roots().state().join("host-lifecycle");
    let Some(mut record) = Shutdown::read(&root)? else {
        return Ok(());
    };
    if record.phase == Phase::Stopping {
        return Err(Error::Operation { message: "host shutdown is incomplete; repeat `wrix service stop --host` before ordinary startup".into() });
    }
    let _pools = crate::nix_store::lifecycle::gate::exclusive(host).map_err(io::Error::other)?;
    crate::nix_store::lifecycle::gate::check_clear(host).map_err(io::Error::other)?;
    let guard = coordination::acquire(&root)?;
    let selected = recorded_owner(host)?;
    let package = retained_package(host)?;
    record.check(host, &selected, &package)?;
    let current = Shutdown::read(guard.root())?.ok_or(owner::Error::Conflict)?;
    if current != record {
        return Err(owner::Error::Conflict.into());
    }
    let runtime = runtime(&selected)?;
    validate_inventory(&runtime, &selected)?;
    let layout = wrix_cache::backend::layout::Layout::current()?;
    let pins = ownership::Pin::capture(host.roots().state(), layout.metadata())?;
    if pins.iter().map(ownership::Pin::record).collect::<Vec<_>>() != record.locks {
        return Err(Error::Operation {
            message:
                "selected storage ownership inodes changed while stopped; no backend was opened"
                    .into(),
        });
    }
    drop(pins);
    record.phase = Phase::Resuming;
    record.publish(guard.root())?;
    let previous = owner::Selected::read(guard.root())?;
    owner::host::ensure(&runtime, &guard, previous.as_ref(), None, true)?;
    let controller =
        Lifecycle::lock(&host.roots().state().join("control")).map_err(io::Error::other)?;
    controller
        .start(&native(host, &package)?, &package)
        .map_err(io::Error::other)?;
    crate::control::maintenance::ready(host).map_err(io::Error::other)?;
    fs::remove_file(root.join(SHUTDOWN))?;
    File::open(root)?.sync_all()?;
    Ok(())
}

pub(super) fn allows_update(state: &Path, candidate: &super::binding::Candidate) -> Result<bool> {
    update::allows(state, candidate)
}

pub fn status() -> Result<String> {
    let host = Host::discover().map_err(io::Error::other)?;
    let Some(deployment) = crate::control::existing_deployment(&host).map_err(io::Error::other)?
    else {
        return Ok("scope: host-user\ndeployment: absent\n".into());
    };
    let root = host.roots().state().join("host-lifecycle");
    let shutdown = Shutdown::read(&root)?;
    let selected = owner::Selected::read(&root)?;
    Ok(format!(
        "scope: host-user\ndeployment: {deployment}\nselected container: {}\nshutdown: {:?}\n{}{}",
        selected
            .as_ref()
            .map_or("unrecorded", |selected| selected.container.as_str()),
        shutdown.as_ref().map(|record| record.phase),
        update::status(&root)?,
        super::generation::status(host.roots().state())?
    ))
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn passive_shutdown_lookup_does_not_create_directories() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            Shutdown::read(&root.path().join("absent"))
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn shutdown_document_rejects_symlinks_without_reading_the_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("foreign");
        fs::write(&target, b"private payload").unwrap();
        std::os::unix::fs::symlink(&target, root.path().join(SHUTDOWN)).unwrap();
        assert!(Shutdown::read(root.path()).is_err());
        assert_eq!(fs::read(target).unwrap(), b"private payload");
    }
}
