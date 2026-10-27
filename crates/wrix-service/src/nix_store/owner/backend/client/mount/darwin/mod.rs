//! Host-native macFUSE identity. A path, reused device number or lease name alone is insufficient.
use super::{Error, lease};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    fsid: String,
    device: u64,
    inode: u64,
    born: (i64, i64),
    owner: u32,
    mountpoint: Vec<u8>,
    source: Vec<u8>,
}

#[cfg(not(target_os = "macos"))]
pub(super) const fn observe(_: &Path, _: &lease::Id) -> Result<Option<Identity>, Error> {
    Err(Error::Unsupported)
}

#[cfg(target_os = "macos")]
fn string(bytes: &[libc::c_char]) -> Result<Vec<u8>, Error> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Error::Invalid)?;
    Ok(bytes[..end]
        .iter()
        .map(|byte| byte.to_ne_bytes()[0])
        .collect())
}

#[cfg(target_os = "macos")]
fn directory(path: &Path) -> Result<std::os::fd::OwnedFd, Error> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut fd = open("/", flags, Mode::empty()).map_err(std::io::Error::from)?;
    for part in path.components() {
        match part {
            std::path::Component::RootDir => (),
            std::path::Component::Normal(name) => {
                fd = openat(&fd, name, flags, Mode::empty()).map_err(std::io::Error::from)?
            }
            _ => return Err(Error::Invalid),
        }
    }
    Ok(fd)
}

#[cfg(target_os = "macos")]
pub(super) fn observe(path: &Path, lease: &lease::Id) -> Result<Option<Identity>, Error> {
    use std::os::unix::ffi::OsStrExt;
    let fd = directory(path)?;
    let metadata = rustix::fs::fstat(&fd).map_err(std::io::Error::from)?;
    let native = rustix::fs::fstatfs(&fd).map_err(std::io::Error::from)?;
    let current = rustix::fs::statfs(path).map_err(std::io::Error::from)?;
    if native.f_fsid != current.f_fsid
        || native.f_mntonname != current.f_mntonname
        || native.f_mntfromname != current.f_mntfromname
        || native.f_flags != current.f_flags
        || native.f_owner != current.f_owner
    {
        return Err(Error::Retained);
    }
    let mountpoint = string(&native.f_mntonname)?;
    if mountpoint != path.as_os_str().as_bytes() {
        return Ok(None);
    }
    let source = string(&native.f_mntfromname)?;
    let kind = string(&native.f_fstypename)?;
    let required = u32::try_from(libc::MNT_RDONLY | libc::MNT_NODEV | libc::MNT_NOSUID)
        .map_err(|_| Error::Invalid)?;
    if ![
        b"macfuse".as_slice(),
        b"osxfuse".as_slice(),
        b"fusefs".as_slice(),
    ]
    .contains(&kind.as_slice())
        || source != format!("wrix-nix-{}", lease.as_str()).as_bytes()
        || native.f_owner != rustix::process::geteuid().as_raw()
        || native.f_flags & required != required
        || metadata.st_dev != rustix::fs::stat(path).map_err(std::io::Error::from)?.st_dev
        || metadata.st_ino != rustix::fs::stat(path).map_err(std::io::Error::from)?.st_ino
    {
        return Err(Error::Retained);
    }
    Ok(Some(Identity {
        fsid: format!("{:?}", native.f_fsid),
        device: u64::from(u32::from_ne_bytes(metadata.st_dev.to_ne_bytes())),
        inode: metadata.st_ino,
        born: (metadata.st_birthtime, metadata.st_birthtime_nsec),
        owner: native.f_owner,
        mountpoint,
        source,
    }))
}
