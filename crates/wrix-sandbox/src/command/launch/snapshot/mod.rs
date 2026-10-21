//! Descriptor-relative snapshots do not dereference replaceable worker-controlled entries.

use rustix::fs::{Dir, Mode, OFlags, openat, readlinkat};
use std::{
    fs::{self, File},
    io,
    os::unix::{ffi::OsStrExt, fs::symlink},
    path::Path,
};
use wrix_core::host_path::{MountAccess, Roots};

pub(super) fn copy_dir(source: &Path, target: &Path) -> io::Result<()> {
    let roots = Roots::discover()?;
    let source_path = source.canonicalize()?;
    let target_path = target
        .parent()
        .ok_or_else(|| io::Error::other("invalid snapshot target"))?
        .canonicalize()?
        .join(
            target
                .file_name()
                .ok_or_else(|| io::Error::other("invalid snapshot target"))?,
        );
    if target_path.starts_with(&source_path) {
        return Err(io::Error::other("snapshot target is inside its source"));
    }
    let source = roots.open_mount(&source_path, MountAccess::Read)?;
    if target.try_exists()? {
        fs::remove_dir_all(target)?;
    }
    if !source.metadata()?.is_dir() {
        return Err(io::Error::other("mount directory changed type"));
    }
    copy_directory(&roots, &source, &source_path, target)
}

fn copy_directory(
    roots: &Roots,
    source: &File,
    source_path: &Path,
    target: &Path,
) -> io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in Dir::read_from(source)? {
        let entry = entry?;
        if matches!(entry.file_name().to_bytes(), b"." | b"..") {
            continue;
        }
        let target = target.join(std::ffi::OsStr::from_bytes(entry.file_name().to_bytes()));
        let handle = openat(
            source,
            entry.file_name(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        );
        match handle {
            Ok(handle) => copy_handle(
                roots,
                &mut File::from(handle),
                &source_path.join(std::ffi::OsStr::from_bytes(entry.file_name().to_bytes())),
                &target,
            )?,
            Err(error) if error == rustix::io::Errno::LOOP => {
                let link = readlinkat(source, entry.file_name(), Vec::new())?;
                let link = Path::new(std::ffi::OsStr::from_bytes(link.to_bytes()));
                let candidate = if link.is_absolute() {
                    Some(link.to_path_buf())
                } else if source_path.starts_with("/nix/store") {
                    Some(source_path.join(link))
                } else {
                    None
                };
                if let Some(candidate) = candidate {
                    match candidate.canonicalize() {
                        Ok(candidate) => {
                            roots.protect_mount(&candidate)?;
                            if candidate.starts_with("/nix/store") {
                                let mut handle = roots.open_mount(&candidate, MountAccess::Read)?;
                                copy_handle(roots, &mut handle, &candidate, &target)?;
                                continue;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                        Err(error) => return Err(error),
                    }
                }
                symlink(link, target)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn copy_handle(
    roots: &Roots,
    source: &mut File,
    source_path: &Path,
    target: &Path,
) -> io::Result<()> {
    let metadata = source.metadata()?;
    if metadata.is_dir() {
        copy_directory(roots, source, source_path, target)
    } else if metadata.is_file() {
        io::copy(source, &mut File::create(target)?)?;
        fs::set_permissions(target, metadata.permissions())
    } else {
        Err(io::Error::other("unsupported mount snapshot entry"))
    }
}
