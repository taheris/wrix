use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use fs2::FileExt;

use super::{LaunchError, MountMode, RenderedMount};

pub(super) const CONTAINER_FILE: &str = "/mnt/wrix/pi-agent-auth/auth.json";
const STORE_SUFFIX: &str = ".wrix-auth";

pub(super) struct Storage {
    directory: PathBuf,
}

impl Storage {
    /// Preserve the selected path while exposing only durable credentials and Pi's lock.
    pub(super) fn prepare(path: &Path, allow_create: bool) -> Result<Self, LaunchError> {
        Self::prepare_inner(path, allow_create, 0)
    }

    fn prepare_inner(path: &Path, allow_create: bool, depth: usize) -> Result<Self, LaunchError> {
        if depth >= 40 {
            return Err(invalid(path));
        }
        let path = std::path::absolute(path)?;
        let parent = path.parent().ok_or_else(|| invalid(&path))?;
        if allow_create {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        let name = path.file_name().ok_or_else(|| invalid(&path))?;
        let mut store_name = name.to_os_string();
        store_name.push(STORE_SUFFIX);
        let directory = parent.join(store_name);
        let target = directory.join("auth.json");
        if is_store_file(&path) {
            return Self::open(parent);
        }
        if let Some(link) = selected_link(&path)? {
            let link = if link.is_absolute() {
                link
            } else {
                parent.join(link)
            };
            if link == target || is_store_file(&link) {
                return Self::open(link.parent().ok_or_else(|| invalid(&link))?);
            }
            return Self::prepare_inner(&link, allow_create, depth + 1);
        }
        if !entry_exists(&path)? && !entry_exists(&target)? && !allow_create {
            return Err(LaunchError::PiAuthMissing {
                path: path.display().to_string(),
            });
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let mut lock_name = name.to_os_string();
        lock_name.push(".wrix-migration.lock");
        let lock = private_file(&parent.join(lock_name), true)?;
        FileExt::lock_exclusive(&lock)?;
        if let Some(link) = selected_link(&path)? {
            return if link == target {
                Self::open(&directory)
            } else {
                Err(invalid(&path))
            };
        }
        let mut pi_lock_name = name.to_os_string();
        pi_lock_name.push(".lock");
        if entry_exists(&parent.join(pi_lock_name))? {
            return Err(LaunchError::PiAuthMigrationBusy {
                path: path.display().to_string(),
            });
        }
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        validate_directory(&directory)?;
        if path.try_exists()? {
            if entry_exists(&target)? {
                return Err(LaunchError::PiAuthMigrationConflict {
                    path: path.display().to_string(),
                });
            }
            private_file(&path, false)?;
            fs::rename(&path, &target)?;
        } else if !entry_exists(&target)? {
            let mut file = private_file(&target, true)?;
            file.write_all(b"{}\n")?;
            file.sync_all()?;
        }
        let storage = Self::open(&directory)?;
        symlink(&target, &path)?;
        File::open(parent)?.sync_all()?;
        Ok(storage)
    }

    fn open(directory: &Path) -> Result<Self, LaunchError> {
        validate_directory(directory)?;
        private_file(&directory.join("auth.json"), false)?;
        Ok(Self {
            directory: directory.to_path_buf(),
        })
    }

    pub(super) fn mount(&self) -> RenderedMount {
        RenderedMount {
            host: self.directory.display().to_string(),
            container: String::from("/mnt/wrix/pi-agent-auth"),
            mode: MountMode::Rw,
            optional: false,
        }
    }
}

fn is_store_file(path: &Path) -> bool {
    path == Path::new(CONTAINER_FILE)
        || path.file_name().is_some_and(|name| name == "auth.json")
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name.to_string_lossy().ends_with(STORE_SUFFIX))
}

fn invalid(path: &Path) -> LaunchError {
    LaunchError::PiAuthStorageInvalid {
        path: path.display().to_string(),
    }
}

fn private_file(path: &Path, create: bool) -> Result<File, LaunchError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid(path));
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn selected_link(path: &Path) -> Result<Option<PathBuf>, LaunchError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(Some(fs::read_link(path)?)),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn entry_exists(path: &Path) -> Result<bool, LaunchError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn validate_directory(path: &Path) -> Result<(), LaunchError> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)?;
    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        if name != "auth.json" && name != "auth.json.lock" {
            return Err(invalid(path));
        }
        if name == "auth.json.lock" {
            match fs::symlink_metadata(entry.path()) {
                Ok(metadata) if metadata.is_dir() => (),
                Ok(_) => return Err(invalid(&entry.path())),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn migration_preserves_credentials_and_isolates_siblings() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selected.json");
        fs::write(&path, b"existing credentials").unwrap();
        fs::write(root.path().join("sibling"), b"private").unwrap();
        let storage = Storage::prepare(&path, false).unwrap();
        assert!(path.is_symlink());
        assert_eq!(fs::read(&path).unwrap(), b"existing credentials");
        assert_eq!(fs::read_dir(&storage.directory).unwrap().count(), 1);
        assert_eq!(
            fs::metadata(&storage.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(storage.mount().container, "/mnt/wrix/pi-agent-auth");
    }

    #[test]
    fn overlapping_launches_share_updates_without_exit_copyback() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("auth.json");
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| Storage::prepare(&path, true).unwrap());
            }
        });
        let first = Storage::prepare(&path, false).unwrap();
        let second = Storage::prepare(&path, false).unwrap();
        fs::write(first.directory.join("auth.json"), b"refreshed").unwrap();
        drop(second);
        drop(first);
        let restarted = Storage::prepare(&path, false).unwrap();
        assert_eq!(
            fs::read(restarted.directory.join("auth.json")).unwrap(),
            b"refreshed"
        );
        assert_eq!(fs::read(&path).unwrap(), b"refreshed");
    }

    #[test]
    fn interrupted_migration_recovers_without_overwriting_credentials() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("auth.json");
        let storage = Storage::prepare(&path, true).unwrap();
        fs::write(storage.directory.join("auth.json"), b"durable").unwrap();
        fs::remove_file(&path).unwrap();
        Storage::prepare(&path, false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"durable");
    }

    #[test]
    fn unsafe_shared_storage_is_rejected_without_touching_symlink_target() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("auth.json");
        let storage = Storage::prepare(&path, true).unwrap();
        let unrelated = root.path().join("unrelated");
        fs::write(&unrelated, b"private").unwrap();
        fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o640)).unwrap();
        fs::remove_file(storage.directory.join("auth.json")).unwrap();
        symlink(&unrelated, storage.directory.join("auth.json")).unwrap();
        assert!(Storage::prepare(&path, false).is_err());
        assert_eq!(fs::read(&unrelated).unwrap(), b"private");
        assert_eq!(
            fs::metadata(&unrelated).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn migration_refuses_active_pi_and_conflicting_copies() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("auth.json");
        fs::write(&path, b"original").unwrap();
        fs::create_dir(root.path().join("auth.json.lock")).unwrap();
        assert!(matches!(
            Storage::prepare(&path, false),
            Err(LaunchError::PiAuthMigrationBusy { .. })
        ));
        fs::remove_dir(root.path().join("auth.json.lock")).unwrap();
        fs::create_dir(root.path().join("auth.json.wrix-auth")).unwrap();
        fs::write(
            root.path().join("auth.json.wrix-auth/auth.json"),
            b"different",
        )
        .unwrap();
        assert!(matches!(
            Storage::prepare(&path, false),
            Err(LaunchError::PiAuthMigrationConflict { .. })
        ));
        assert_eq!(fs::read(path).unwrap(), b"original");
    }

    #[test]
    fn unexpected_store_entries_are_not_mounted() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("auth.json");
        let storage = Storage::prepare(&path, true).unwrap();
        fs::write(storage.directory.join("settings.json"), b"unrelated").unwrap();
        assert!(Storage::prepare(&path, false).is_err());
    }

    #[test]
    fn credential_replacement_remains_visible_through_selected_path() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("auth.json");
        let storage = Storage::prepare(&path, true).unwrap();
        let replacement = storage.directory.join("replacement");
        fs::write(&replacement, b"refreshed").unwrap();
        fs::rename(replacement, storage.directory.join("auth.json")).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"refreshed");
        assert_eq!(
            Storage::prepare(&path, false).unwrap().directory,
            storage.directory
        );
    }

    #[test]
    fn selected_alias_reuses_storage_and_missing_auth_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selected.json");
        let alias = root.path().join("alias.json");
        assert!(Storage::prepare(&path, false).is_err());
        fs::write(&path, b"existing").unwrap();
        symlink(&path, &alias).unwrap();
        let first = Storage::prepare(&alias, false).unwrap();
        let second = Storage::prepare(&path, false).unwrap();
        assert_eq!(first.directory, second.directory);
    }
}
