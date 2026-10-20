//! Namespace publication retirement without deleting backend data or signing state.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::Duration,
};

use wrix_core::path::WorkspaceHash;

use super::{Error, Result, private_directory};

const NAME: &str = "celler-retired";
const MAX_BYTES: u64 = 128;

/// Reads host admission only; absence preserves previously installed bindings.
pub fn is_retired(root: &Path, workspace: &WorkspaceHash) -> Result<bool> {
    match fs::symlink_metadata(root.join(NAME)) {
        Ok(_) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    let directory = match private_directory(root) {
        Ok(directory) => directory,
        Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(NAME))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.uid() != directory.uid()
    {
        return Err(Error::Permissions);
    }
    let mut contents = String::new();
    file.take(MAX_BYTES + 1).read_to_string(&mut contents)?;
    if contents != record(workspace) {
        return Err(Error::Binding);
    }
    Ok(true)
}

/// Serializes host publication and closes future admission without claiming backend release.
pub async fn retire(root: &Path, workspace: &WorkspaceHash, timeout: Duration) -> Result<()> {
    let _lock = super::registration::acquire(root, timeout).await?;
    if is_retired(root, workspace)? {
        return Ok(());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    temporary.write_all(record(workspace).as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(root.join(NAME))
        .map_err(|error| error.error)?;
    File::open(root)?
        .sync_all()
        .map_err(Error::RetirementCommitted)?;
    Ok(())
}

/// Reopens only after authenticated registration while holding the namespace lock.
pub(super) fn activate(root: &Path, workspace: &WorkspaceHash) -> Result<()> {
    if is_retired(root, workspace)? {
        fs::remove_file(root.join(NAME))?;
        File::open(root)?
            .sync_all()
            .map_err(Error::InstallationCommitted)?;
    }
    Ok(())
}

fn record(workspace: &WorkspaceHash) -> String {
    format!("wrix-celler-retired-v1\n{}\n", workspace.as_str())
}

#[cfg(test)]
mod test {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn workspace(root: &Path) -> WorkspaceHash {
        wrix_core::path::Workspace::from_path(root)
            .unwrap()
            .hash()
            .clone()
    }

    #[tokio::test]
    async fn retirement_is_scoped_idempotent_and_preserves_authority_and_keys() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let identity = workspace(root.path());
        fs::write(
            root.path().join("celler-publisher.json"),
            "retained authority",
        )
        .unwrap();
        fs::write(root.path().join("cache.pub"), "retained key").unwrap();
        assert!(!is_retired(root.path(), &identity).unwrap());
        retire(root.path(), &identity, Duration::ZERO)
            .await
            .unwrap();
        retire(root.path(), &identity, Duration::ZERO)
            .await
            .unwrap();
        assert!(is_retired(root.path(), &identity).unwrap());
        assert_eq!(
            fs::read_to_string(root.path().join("cache.pub")).unwrap(),
            "retained key"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("celler-publisher.json")).unwrap(),
            "retained authority"
        );
        let peer = tempfile::tempdir().unwrap();
        assert!(matches!(
            is_retired(root.path(), &workspace(peer.path())),
            Err(Error::Binding)
        ));
        activate(root.path(), &identity).unwrap();
        assert!(!is_retired(root.path(), &identity).unwrap());
    }

    #[tokio::test]
    async fn held_publication_lock_prevents_retirement_without_dispatch_or_mutation() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let identity = workspace(root.path());
        let _lock = super::super::registration::acquire(root.path(), Duration::ZERO)
            .await
            .unwrap();
        assert!(matches!(
            retire(root.path(), &identity, Duration::ZERO).await,
            Err(Error::LockTimeout)
        ));
        assert!(!root.path().join(NAME).exists());
    }

    #[tokio::test]
    async fn unsafe_retirement_marker_cannot_be_read_or_replaced() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let identity = workspace(root.path());
        let target = root.path().join("unrelated");
        fs::write(&target, "preserve").unwrap();
        symlink(&target, root.path().join(NAME)).unwrap();
        assert!(is_retired(root.path(), &identity).is_err());
        assert!(
            retire(root.path(), &identity, Duration::ZERO)
                .await
                .is_err()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "preserve");
    }
}
