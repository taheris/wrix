//! Host-user lifecycle exclusion, separate from backend process-lifetime ownership.

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

use displaydoc::Display;
use fs2::FileExt;
use thiserror::Error as ThisError;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// cannot prepare host lifecycle coordination: {0}
    Directory(#[from] wrix_core::private_directory::Error),
    /// cannot acquire host lifecycle coordination: {0}
    Io(#[from] io::Error),
    /// host lifecycle lock must be an owner-only regular file with one link
    Permissions,
}

/// Serializes cooperating host mutations; this is not backend quiescence or release evidence.
/// Callers retain the file through inspection, layout changes and runtime mutation.
pub fn acquire(root: &Path) -> Result<File, Error> {
    wrix_core::private_directory::prepare_with_parents(root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join("lifecycle.lock"))?;
    let metadata = lock.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != fs::metadata(root)?.uid()
        || metadata.nlink() != 1
    {
        return Err(Error::Permissions);
    }
    lock.lock_exclusive()?;
    Ok(lock)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn guard_holds_exclusion_until_drop_and_preserves_lock_inode() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("host");
        let first = acquire(&root).unwrap();
        let path = root.join("lifecycle.lock");
        let inode = first.metadata().unwrap().ino();
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert_eq!(
            contender.try_lock_exclusive().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(first);
        contender.try_lock_exclusive().unwrap();
        drop(contender);
        let second = acquire(&root).unwrap();
        assert_eq!(second.metadata().unwrap().ino(), inode);
        assert_eq!(second.metadata().unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(root).unwrap().mode() & 0o777, 0o700);
    }

    #[test]
    fn rejects_symlink_without_touching_target() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("host");
        fs::create_dir(&root).unwrap();
        let target = directory.path().join("unrelated");
        fs::write(&target, "preserve").unwrap();
        symlink(&target, root.join("lifecycle.lock")).unwrap();
        assert!(matches!(acquire(&root), Err(Error::Io(_))));
        assert_eq!(fs::read_to_string(target).unwrap(), "preserve");
    }

    #[test]
    fn rejects_hard_link_without_changing_target() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("host");
        fs::create_dir(&root).unwrap();
        let target = directory.path().join("unrelated");
        fs::write(&target, "preserve").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&target, root.join("lifecycle.lock")).unwrap();
        assert!(matches!(acquire(&root), Err(Error::Permissions)));
        assert_eq!(fs::read_to_string(target).unwrap(), "preserve");
    }

    #[test]
    fn rejects_nonprivate_lock_without_rewriting_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("host");
        fs::create_dir(&root).unwrap();
        let path = root.join("lifecycle.lock");
        fs::write(&path, "preserve").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(acquire(&root), Err(Error::Permissions)));
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o644);
        assert_eq!(fs::read_to_string(path).unwrap(), "preserve");
    }
}
