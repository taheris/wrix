//! Bounded host-side writer admission without SQLite's synchronized busy backoff.
use super::Error;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::{Duration, Instant},
};

pub(super) fn acquire(directory: &Path) -> Result<File, Error> {
    acquire_before(directory, Instant::now() + Duration::from_secs(30))
}

fn acquire_before(directory: &Path, deadline: Instant) -> Result<File, Error> {
    fs::create_dir_all(directory)?;
    let flags =
        i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).map_err(|_| Error::PrivateState)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(flags)
        .open(directory.join("attention.lock"))?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(Error::PrivateState);
    }
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(Error::Busy);
                }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn active_writer_times_out_without_replacing_its_lock() {
        let root = tempfile::tempdir().unwrap();
        let held = acquire(root.path()).unwrap();
        assert!(matches!(
            acquire_before(root.path(), Instant::now() + Duration::from_millis(30)),
            Err(Error::Busy)
        ));
        assert!(matches!(held.try_lock(), Ok(())));
        drop(held);
        acquire(root.path()).unwrap();
    }

    #[test]
    fn writer_lock_rejects_symlinks_hardlinks_and_shared_permissions() {
        let root = tempfile::tempdir().unwrap();
        let other = root.path().join("other");
        fs::write(&other, b"unchanged").unwrap();
        let lock = root.path().join("attention.lock");
        symlink(&other, &lock).unwrap();
        assert!(acquire(root.path()).is_err());
        fs::remove_file(&lock).unwrap();
        fs::hard_link(&other, &lock).unwrap();
        assert!(matches!(acquire(root.path()), Err(Error::PrivateState)));
        fs::remove_file(&lock).unwrap();
        fs::write(&lock, []).unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(acquire(root.path()), Err(Error::PrivateState)));
        assert_eq!(fs::read(other).unwrap(), b"unchanged");
    }
}
