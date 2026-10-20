//! Native process-lifetime exclusion for the Dolt component epoch and its descendants.

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    process::Stdio,
};

use fs2::FileExt;
use tokio::process::{Child, Command};

use super::Error;

const FILE: &str = "dolt-epoch.lock";

pub(super) struct Held(File);

impl Held {
    /// Busy is an observed lock holder, not permission to cancel or replay its work.
    pub fn acquire(root: &Path) -> Result<Option<Self>, Error> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join(FILE))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != fs::symlink_metadata(root)?.uid()
        {
            return Err(Error::Permissions);
        }
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self(file))),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// The caller retains supervisor ownership across this handoff; competing native holders fail busy.
    /// The child alone exports its guard to descendants, never unrelated controller forks.
    pub fn spawn(self, root: &Path, recipe: &str) -> Result<Child, Error> {
        drop(self.0);
        Ok(Command::new("flock")
            .args([
                "--exclusive",
                "--wait",
                "1",
                "--conflict-exit-code",
                "75",
                "--no-fork",
            ])
            .arg(root.join(FILE))
            .args(["sh", "-c", recipe])
            .stdin(Stdio::null())
            .kill_on_drop(false)
            .spawn()?)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod test {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn child_owned_guard_excludes_new_epochs_until_native_process_exit() {
        let root = tempfile::tempdir().unwrap();
        let guard = Held::acquire(root.path()).unwrap().unwrap();
        let recipe = format!(
            "touch '{}'; while test ! -f '{}'; do sleep 0.01; done",
            root.path().join("ready").display(),
            root.path().join("release").display()
        );
        let mut child = guard.spawn(root.path(), &recipe).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !root.path().join("ready").exists() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(Held::acquire(root.path()).unwrap().is_none());
        fs::write(root.path().join("release"), "complete owned fixture").unwrap();
        child.wait().await.unwrap();
        assert!(Held::acquire(root.path()).unwrap().is_some());
    }
}
