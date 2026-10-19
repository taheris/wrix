//! Owner-private host state directories; existing contents are preserved.

use std::{
    fs::{self, DirBuilder, OpenOptions, Permissions},
    io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

use displaydoc::Display;
use thiserror::Error as ThisError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// private directory has no parent
    Parent,
    /// cannot prepare owner-private state directory: {0}
    Io(#[from] io::Error),
}

/// Creates or tightens the selected directory without following a final symlink.
/// Parent directories are host-selected and must already exist.
pub fn prepare(path: &Path) -> Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)?;
    directory.set_permissions(Permissions::from_mode(0o700))?;
    directory.sync_all()?;
    Ok(())
}

/// Creates host-selected parents before preparing the private leaf directory.
pub fn prepare_with_parents(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or(Error::Parent)?;
    fs::create_dir_all(parent)?;
    prepare(path)
}

#[cfg(test)]
mod test {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    use super::*;

    #[test]
    fn new_directory_is_private() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("control");
        prepare(&path).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn existing_directory_is_tightened_without_changing_contents() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("control");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
        fs::write(path.join("manifest.json"), "preserved").unwrap();
        prepare(&path).unwrap();
        prepare(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::read_to_string(path.join("manifest.json")).unwrap(),
            "preserved"
        );
    }

    #[test]
    fn symlink_is_rejected_without_changing_its_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("unrelated");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, Permissions::from_mode(0o755)).unwrap();
        let link = root.path().join("control");
        symlink(&target, &link).unwrap();
        assert!(prepare(&link).is_err());
        assert!(fs::symlink_metadata(link).unwrap().is_symlink());
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn ordinary_file_is_rejected_and_preserved() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("control");
        fs::write(&path, "preserved").unwrap();
        assert!(prepare(&path).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "preserved");
    }
}
