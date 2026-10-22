//! Mediate untrusted Git snapshots without ever giving native root SQL a
//! worker-writable file URL. All projected traversal is descriptor-relative.

use super::Error;
use fs2::FileExt;
use rustix::fs::{self as at, Dir, Mode, OFlags};
use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Write},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path},
};
use wrix_core::git::Branch;

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn directory(parent: &File, path: &Path, create: bool) -> Result<File, Error> {
    let mut current = parent.try_clone()?;
    for part in path.components() {
        let Component::Normal(name) = part else {
            return Err(Error::UnsafePath);
        };
        if create {
            match at::mkdirat(&current, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => (),
                Err(error) => return Err(std::io::Error::from(error).into()),
            }
        }
        current = File::from(at::openat(&current, name, DIRECTORY, Mode::empty())?);
    }
    Ok(current)
}

pub(super) fn transfer(namespace: &Path, branch: &Branch, publish: bool) -> Result<(), Error> {
    super::check_directory(namespace)?;
    let root = File::from(at::open(namespace, DIRECTORY, Mode::empty())?);
    let private = directory(&root, Path::new("native-remote"), true)?;
    let lock = File::from(at::openat(
        &private,
        "LOCK",
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?);
    if !lock.metadata()?.is_file() || lock.metadata()?.nlink() != 1 {
        return Err(Error::UnsafePath);
    }
    lock.try_lock_exclusive()?;
    let projected_path = Path::new("beads-sync")
        .join(branch.as_str())
        .join(".beads/dolt-remote");
    let projected = directory(&root, &projected_path, publish)?;
    let (source, destination) = if publish {
        (&private, &projected)
    } else {
        (&projected, &private)
    };
    // Absence is not an empty snapshot: importing it would silently reuse a
    // stale private manifest. Open without following even a leaf symlink.
    let manifest = File::from(at::openat(
        source,
        "manifest",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if !manifest.metadata()?.is_file() || manifest.metadata()?.nlink() != 1 {
        return Err(Error::UnsafePath);
    }
    let mut budget = Budget {
        bytes: 1 << 30,
        entries: 100_000,
    };
    let result = copy(source, destination, &mut budget, 0);
    FileExt::unlock(&lock)?;
    result
}

/// Copy raw legacy storage into a new host-private inode graph. A rename does
/// not revoke a sandbox's retained directory descriptors; privileged native
/// backup must never traverse that original graph after a mere pathname check.
pub(super) fn source_snapshot(source: &Path, target: &Path) -> Result<(), Error> {
    super::check_directory(source)?;
    super::private_directory(target)?;
    let source = File::from(at::open(source, DIRECTORY, Mode::empty())?);
    let target = File::from(at::open(target, DIRECTORY, Mode::empty())?);
    copy_raw(
        &source,
        &target,
        &mut Budget {
            bytes: 16 << 30,
            entries: 1_000_000,
        },
        0,
    )
}

fn copy_raw(
    source: &File,
    destination: &File,
    budget: &mut Budget,
    depth: usize,
) -> Result<(), Error> {
    if depth > 32 {
        return Err(Error::UnsafePath);
    }
    for entry in Dir::read_from(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        budget.entries = budget.entries.checked_sub(1).ok_or(Error::UnsafePath)?;
        let name = OsStr::from_bytes(name.to_bytes());
        let opened = File::from(at::openat(
            source,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let metadata = opened.metadata()?;
        if metadata.is_dir() {
            let target = directory(destination, Path::new(name), true)?;
            copy_raw(&opened, &target, budget, depth + 1)?;
        } else if metadata.is_file() && metadata.nlink() == 1 {
            // Never inherit server auto-connection metadata or the old writer
            // lock inode. The private graph has a new lock created by its owner.
            if matches!(name.to_str(), Some("LOCK" | "sql-server.lock")) {
                continue;
            }
            if metadata.len() > budget.bytes {
                return Err(Error::UnsafePath);
            }
            let mut output = File::from(at::openat(
                destination,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?);
            let copied = std::io::copy(&mut opened.take(budget.bytes + 1), &mut output)?;
            budget.bytes = budget.bytes.checked_sub(copied).ok_or(Error::UnsafePath)?;
            output.sync_all()?;
        } else {
            return Err(Error::UnsafePath);
        }
    }
    destination.sync_all()?;
    Ok(())
}

struct Budget {
    bytes: u64,
    entries: usize,
}

fn copy(source: &File, destination: &File, budget: &mut Budget, depth: usize) -> Result<(), Error> {
    if depth > 32 {
        return Err(Error::UnsafePath);
    }
    let mut names = Vec::new();
    for entry in Dir::read_from(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        budget.entries = budget.entries.checked_sub(1).ok_or(Error::UnsafePath)?;
        names.push(name.to_owned());
    }
    if depth == 0 && !names.iter().any(|name| name.to_bytes() == b"manifest") {
        return Err(Error::UnsafePath);
    }
    // Native table data is immutable; publish the manifest only after all data.
    names.sort_by_key(|name| name.to_bytes() == b"manifest");
    for name in names {
        let name = OsStr::from_bytes(name.to_bytes());
        let opened = File::from(at::openat(
            source,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let metadata = opened.metadata()?;
        if metadata.is_dir() {
            if name == "manifest" || name == "LOCK" {
                return Err(Error::UnsafePath);
            }
            let target = directory(destination, Path::new(name), true)?;
            copy(&opened, &target, budget, depth + 1)?;
        } else if metadata.is_file() && metadata.nlink() == 1 {
            // Never delete, overwrite or replace Dolt's native writer lock inode.
            if name == "LOCK" {
                continue;
            }
            if metadata.len() > budget.bytes {
                return Err(Error::UnsafePath);
            }
            let temporary = format!(".wrix-snapshot-{}", super::random_password()?);
            let mut output = File::from(at::openat(
                destination,
                temporary.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?);
            // Bound actual reads too: a concurrently growing guest file cannot
            // defeat the metadata-size budget. No source bytes are executed.
            let copied = std::io::copy(&mut opened.take(budget.bytes + 1), &mut output)?;
            if copied > budget.bytes {
                return Err(Error::UnsafePath);
            }
            budget.bytes -= copied;
            output.flush()?;
            output.sync_all()?;
            at::renameat(destination, temporary.as_str(), destination, name)?;
        } else {
            return Err(Error::UnsafePath);
        }
    }
    destination.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod test;
