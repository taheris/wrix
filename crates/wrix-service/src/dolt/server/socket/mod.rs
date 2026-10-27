//! Retire only a dead, privately owned Unix endpoint under service ownership exclusion.
use crate::dolt::Error;
use fs2::FileExt;
use std::{
    fs::{self, File},
    io,
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    path::Path,
};

fn connect(path: &Path) -> io::Result<()> {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
    let socket = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    let address = SocketAddrUnix::new(path)?;
    rustix::net::connect(&socket, &address)?;
    Ok(())
}

pub fn ready(data: &Path) -> bool {
    connect(&data.join("run/dolt.sock")).is_ok()
}

/// Used only after verified service stop, while the host lifecycle guard is held.
pub fn retire(data: &Path) -> Result<(), Error> {
    super::super::private_directory(data)?;
    let owner = File::options()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(data.join("owner.lock"))?;
    let metadata = owner.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != fs::metadata(data)?.uid() {
        return Err(Error::UnsafePath);
    }
    owner.try_lock_exclusive()?;
    clear(data)
}

/// The caller holds the original service owner.lock, not a replacement inode.
pub(super) fn clear(data: &Path) -> Result<(), Error> {
    clear_path(data, &data.join("run/dolt.sock"))
}

/// Only the native supervisor holding the original owner inode may retire this endpoint.
pub(super) fn clear_path(data: &Path, path: &Path) -> Result<(), Error> {
    super::super::check_directory(path.parent().ok_or(Error::UnsafePath)?)?;
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !before.file_type().is_socket()
        || before.nlink() != 1
        || before.uid() != fs::metadata(data)?.uid()
    {
        return Err(Error::UnsafePath);
    }
    match connect(path) {
        Ok(()) => {
            return Err(Error::SocketBusy {
                path: path.to_owned(),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
        Err(error) => return Err(error.into()),
    }
    let after = fs::symlink_metadata(path)?;
    if after.dev() != before.dev()
        || after.ino() != before.ino()
        || !after.file_type().is_socket()
        || after.uid() != before.uid()
        || after.nlink() != 1
    {
        return Err(Error::UnsafePath);
    }
    fs::remove_file(path)?;
    Ok(())
}

#[cfg(test)]
mod test;
