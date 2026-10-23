//! Inactive-owner rename with a durable, fail-closed interruption journal.

use super::{Selected, host};
use crate::lifecycle::{Error, Result, Runtime, RuntimeKind, RuntimeStatus, coordination};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    process::Command,
};
use wrix_core::path::ContainerName;

const DIRECTORY: &str = "owner-rename";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
struct NativeId(String);
impl TryFrom<String> for NativeId {
    type Error = super::Error;
    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(super::Error::Invalid);
        }
        Ok(Self(value))
    }
}
impl From<NativeId> for String {
    fn from(value: NativeId) -> Self {
        value.0
    }
}

pub(super) fn pending(root: &Path) -> std::result::Result<bool, super::Error> {
    match fs::symlink_metadata(root.join(DIRECTORY)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

struct Journal {
    previous: Selected,
    target: Selected,
    native: Option<NativeId>,
}
impl Journal {
    fn read(root: &Path) -> Result<Self> {
        let previous = Selected::read_during_rename(root)?.ok_or(super::Error::Invalid)?;
        let target =
            Selected::read_during_rename(&root.join("target"))?.ok_or(super::Error::Invalid)?;
        let file = fs::File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join("native.json"))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != fs::metadata(root)?.uid()
            || metadata.len() > 128
        {
            return Err(super::Error::Permissions.into());
        }
        let mut bytes = Vec::new();
        file.take(129).read_to_end(&mut bytes)?;
        if bytes.len() > 128 {
            return Err(super::Error::Invalid.into());
        }
        let native = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let mut comparison = target.clone();
        comparison.container = previous.container.clone();
        if comparison != previous || target.container == previous.container {
            return Err(super::Error::Invalid.into());
        }
        Ok(Self {
            previous,
            target,
            native,
        })
    }

    fn persist(&self, root: &Path) -> Result<()> {
        let temporary = tempfile::Builder::new()
            .prefix(".owner-rename-")
            .tempdir_in(root)?;
        self.previous.persist(temporary.path())?;
        let target = temporary.path().join("target");
        wrix_core::private_directory::prepare_with_parents(&target)?;
        self.target.persist(&target)?;
        crate::lifecycle::managed::atomic_write(
            &temporary.path().join("native.json"),
            &serde_json::to_vec(&self.native).map_err(io::Error::other)?,
        )?;
        fs::File::open(temporary.path())?.sync_all()?;
        fs::rename(temporary.path(), root.join(DIRECTORY))?;
        fs::File::open(root)?.sync_all()?;
        Ok(())
    }
}

fn identity(runtime: &Runtime, name: &ContainerName) -> Result<NativeId> {
    runtime
        .inspect_format(name.as_str(), "{{.Id}}")?
        .ok_or(super::Error::Invalid)?
        .try_into()
        .map_err(Into::into)
}

fn inactive(runtime: &Runtime, owner: &Selected) -> Result<RuntimeStatus> {
    let status = runtime.status(&owner.container)?;
    if status == RuntimeStatus::Running {
        return Err(Error::Operation {
            message: format!(
                "owner {} is running; explicitly stop it for host-wide rename maintenance first",
                owner.container
            ),
        });
    }
    if status == RuntimeStatus::Stopped {
        host::validate(runtime, owner, status)?;
    }
    Ok(status)
}

pub(in crate::lifecycle) fn run(name: &ContainerName) -> Result<String> {
    let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
    let guard = coordination::acquire(&host.roots().state().join("host-lifecycle"))?;
    let previous = Selected::read_during_rename(guard.root())?.ok_or(super::Error::Invalid)?;
    let mut runtime = Runtime::from_env()?;
    if runtime.kind != previous.runtime {
        runtime.kind = previous.runtime;
        runtime.binary = match previous.runtime {
            RuntimeKind::Podman => "podman",
            RuntimeKind::Container => "container",
        }
        .into();
    }
    if Selected::read(&guard.root().join("celler-candidate"))?.is_some() {
        return Err(super::Error::Conflict.into());
    }
    let root = guard.root().parent().ok_or(super::Error::Invalid)?;
    wrix_cache::backend::issuer::Authority::open(&root.join("host-control/celler-issuer"))
        .map_err(crate::lifecycle::cache::Error::from)?;
    let journal = if pending(guard.root())? {
        Journal::read(&guard.root().join(DIRECTORY))?
    } else {
        if previous.container == *name {
            return Ok(format!("Service owner: {name} (unchanged)\n"));
        }
        let status = inactive(&runtime, &previous)?;
        if status == RuntimeStatus::Stopped && runtime.kind != RuntimeKind::Podman {
            return Err(Error::Operation { message: "this runtime does not support verified native rename; an existing container is left untouched".into() });
        }
        collision(&runtime, name, false)?;
        crate::lifecycle::shared::check_inventory(&runtime, Some(&previous), None)?;
        let target = Selected {
            container: name.clone(),
            ..previous.clone()
        };
        let journal = Journal {
            previous: previous.clone(),
            target,
            native: if status == RuntimeStatus::Stopped {
                Some(identity(&runtime, &previous.container)?)
            } else {
                None
            },
        };
        journal.persist(guard.root())?;
        journal
    };
    if journal.target.container != *name
        || (previous != journal.previous && previous != journal.target)
    {
        return Err(super::Error::Conflict.into());
    }
    finish(&runtime, &guard, &journal)?;
    Ok(format!(
        "Service owner renamed to {name}. Data, signing authority and endpoints retained.\nRun `wrix service recover` (or `wrix service repair` if the old controller requires upgrade), then reload the shell.\n"
    ))
}

fn collision(runtime: &Runtime, name: &ContainerName, allow_local: bool) -> Result<()> {
    for (backend, local) in [(&runtime.alternate(), false), (runtime, true)] {
        if let Some(containers) = backend.available_container_infos()?
            && containers
                .iter()
                .any(|container| container.name == name.as_str())
            && !(local && allow_local)
        {
            return Err(super::Error::Conflict.into());
        }
    }
    Ok(())
}

fn finish(runtime: &Runtime, guard: &coordination::Guard, journal: &Journal) -> Result<()> {
    collision(runtime, &journal.target.container, journal.native.is_some())?;
    let old = inactive(runtime, &journal.previous)?;
    let new = inactive(runtime, &journal.target)?;
    match (&journal.native, old, new) {
        (None, RuntimeStatus::Missing, RuntimeStatus::Missing) => (),
        (Some(expected), RuntimeStatus::Stopped, RuntimeStatus::Missing) => {
            if identity(runtime, &journal.previous.container)? != *expected {
                return Err(super::Error::Conflict.into());
            }
            let output = Command::new(&runtime.binary)
                .args([
                    "rename",
                    "--",
                    journal.previous.container.as_str(),
                    journal.target.container.as_str(),
                ])
                .output()?;
            if !output.status.success() {
                return Err(Error::Operation {
                    message: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
            }
            if runtime.status(&journal.previous.container)? != RuntimeStatus::Missing
                || inactive(runtime, &journal.target)? != RuntimeStatus::Stopped
                || identity(runtime, &journal.target.container)? != *expected
            {
                return Err(super::Error::Conflict.into());
            }
        }
        (Some(expected), RuntimeStatus::Missing, RuntimeStatus::Stopped) => {
            if identity(runtime, &journal.target.container)? != *expected {
                return Err(super::Error::Conflict.into());
            }
        }
        _ => return Err(super::Error::Conflict.into()),
    }
    journal.target.persist(guard.root())?;
    // Atomically clear the fence before deleting only the completed journal.
    let completed = tempfile::Builder::new()
        .prefix(".renamed-owner-")
        .tempdir_in(guard.root())?;
    fs::rename(
        guard.root().join(DIRECTORY),
        completed.path().join("journal"),
    )?;
    fs::File::open(guard.root())?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod test;
