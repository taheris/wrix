//! Kernel admission exclusion plus durable interruption fencing; neither is worker exit evidence.
use super::Error;
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
};
use wrix_state::host::Host;

#[cfg(test)]
mod test;

pub const MARKER: &str = "nix-pool-maintenance.json";
pub struct Guard {
    _file: File,
}
fn lock(host: &Host, exclusive: bool) -> Result<Guard, Error> {
    let root = host.roots().state();
    // Never tighten or repair existing authority. A legacy structural parent may be searchable;
    // the lock/receipt files remain private, and pool inventory separately requires private state.
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)?;
    crate::nix_store::owner::check_parent(root)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join("nix-pool-lifecycle.lock"))?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.nlink() != 1
    {
        return Err(Error::Invalid);
    }
    let locked = if exclusive {
        file.try_lock_exclusive()
    } else {
        FileExt::try_lock_shared(&file)
    };
    match locked {
        Ok(()) => Ok(Guard { _file: file }),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Err(Error::Busy),
        Err(error) => Err(error.into()),
    }
}
pub fn check_clear(host: &Host) -> Result<(), Error> {
    match fs::symlink_metadata(host.roots().state().join(MARKER)) {
        Ok(_) => Err(Error::Pending),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
pub fn shared(host: &Host) -> Result<Guard, Error> {
    let guard = lock(host, false)?;
    check_clear(host)?;
    check_services(&host.roots().state().join("host-lifecycle"))?;
    Ok(guard)
}

fn check_services(root: &Path) -> Result<(), Error> {
    for marker in ["shutdown.json", "update.json"] {
        match fs::symlink_metadata(root.join(marker)) {
            Ok(_) => return Err(marker_error(marker)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn marker_error(marker: &str) -> Error {
    if marker == "update.json" {
        Error::UpdatePending
    } else {
        Error::Stopped
    }
}

pub fn exclusive(host: &Host) -> Result<Guard, Error> {
    lock(host, true)
}
pub fn private_file(path: &Path) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.nlink() != 1
    {
        return Err(Error::Invalid);
    }
    Ok(file)
}
