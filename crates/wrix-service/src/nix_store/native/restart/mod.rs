//! Guest-side resume preconditions; host runtime release remains an independent requirement.

#[cfg(test)]
mod test;

use super::{Error, image::StorePath, lease::Key};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

fn owner(path: &Path) -> Result<u32, Error> {
    if path.starts_with(super::DATA) {
        super::bootstrap::ownership::uid(path)
    } else {
        Ok(rustix::process::geteuid().as_raw())
    }
}

fn directory(path: &Path, mode: u32) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != owner(path)?
        || metadata.mode() & 0o777 != mode
        || path.canonicalize()? != path
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>, Error> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != owner(path)?
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > 4096
    {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(Error::Invalid);
    }
    Ok(bytes)
}

pub(super) fn seal(root: &Path, software: &StorePath, admin: &Key) -> Result<(), Error> {
    directory(root, 0o700)?;
    directory(&root.join("owner-active"), 0o700)?;
    for (name, value) in [
        ("software", software.as_str()),
        ("admin-key", admin.as_str()),
    ] {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("owner-active").join(name))?;
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
    }
    File::open(root.join("owner-active"))?.sync_all()?;
    Ok(())
}

/// Replace only the software seal of an explicitly consumed clean checkpoint.
/// The caller has already seeded and rooted the candidate using startup authority.
pub(super) fn upgrade(
    root: &Path,
    previous: &StorePath,
    candidate: &StorePath,
    admin: &Key,
) -> Result<(), Error> {
    directory(root, 0o700)?;
    let owner = root.join("owner-active");
    directory(&owner, 0o700)?;
    if read(&root.join("resuming"))? != b"1\n"
        || read(&owner.join("software"))? != previous.as_str().as_bytes()
        || read(&owner.join("admin-key"))? != admin.as_str().as_bytes()
    {
        return Err(Error::Invalid);
    }
    match fs::symlink_metadata(root.join("released")) {
        Ok(_) => return Err(Error::Invalid),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let mut file = tempfile::NamedTempFile::new_in(&owner)?;
    file.write_all(candidate.as_str().as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(owner.join("software"))
        .map_err(|error| error.error)?;
    File::open(owner)?.sync_all()?;
    Ok(())
}

pub(super) fn begin(root: &Path, software: &StorePath, admin: &Key) -> Result<(), Error> {
    directory(root, 0o700)?;
    for entry in fs::read_dir(root)? {
        let name = entry?.file_name();
        if !matches!(
            name.to_str(),
            Some(
                "ownership.lock"
                    | "bootstrap.json"
                    | "store.ext4"
                    | "owner-active"
                    | "filesystem"
                    | "leases"
                    | "incoming"
                    | "host-key"
                    | "host-key.pub"
                    | "export-root"
                    | "released"
            )
        ) {
            return Err(Error::Invalid);
        }
    }
    directory(&root.join("owner-active"), 0o700)?;
    for entry in fs::read_dir(root.join("owner-active"))? {
        if !matches!(
            entry?.file_name().to_str(),
            Some("version" | "software" | "admin-key")
        ) {
            return Err(Error::Invalid);
        }
    }
    let version = read(&root.join("owner-active/version"))?;
    if !version.starts_with(b"2:") || !version.ends_with(b":8589934592\n") {
        return Err(Error::Invalid);
    }
    directory(&root.join("leases"), 0o700)?;
    directory(&root.join("incoming"), 0o700)?;
    directory(&root.join("filesystem"), 0o700)?;
    directory(&root.join("export-root"), 0o755)?;
    if let Some(entry) = fs::read_dir(root.join("filesystem"))?.next() {
        entry?;
        return Err(Error::Invalid);
    }
    if let Some(entry) = fs::read_dir(root.join("leases"))?.next() {
        entry?;
        return Err(Error::Busy);
    }
    if read(&root.join("owner-active/software"))? != software.as_str().as_bytes()
        || read(&root.join("owner-active/admin-key"))? != admin.as_str().as_bytes()
        || read(&root.join("released"))? != b"1\n"
    {
        return Err(Error::Invalid);
    }
    let mut consumed = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("resuming"))?;
    consumed.write_all(b"1\n")?;
    consumed.sync_all()?;
    File::open(root)?.sync_all()?;
    fs::remove_file(root.join("released"))?;
    File::open(root)?.sync_all()?;
    Ok(())
}
