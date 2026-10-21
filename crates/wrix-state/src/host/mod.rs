//! Host-only capability; no constructor accepts repository or launch authorization.

use std::{fs, path::Path};
use wrix_core::host_path::Roots;

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// workspace authority is host-only and unavailable to workers
    Worker,
    /// host state I/O failed: {0}
    Io(#[from] std::io::Error),
    /// host state must be owned, private and free of symlink substitutions
    PrivateState,
}

pub struct Host {
    roots: Roots,
}

impl Host {
    pub fn discover() -> Result<Self, Error> {
        if Path::new("/etc/wrix/image-agent").try_exists()? {
            return Err(Error::Worker);
        }
        let roots = Roots::discover()?;
        Ok(Self { roots })
    }

    pub const fn roots(&self) -> &Roots {
        &self.roots
    }

    pub(crate) fn check(&self) -> Result<(), Error> {
        if Path::new("/etc/wrix/image-agent").try_exists()? {
            return Err(Error::Worker);
        }
        if !self.roots.state().is_absolute() {
            return Err(Error::PrivateState);
        }
        Ok(())
    }

    pub(crate) fn private_root(&self, create: bool) -> Result<bool, Error> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        self.check()?;
        let root = self.roots.state();
        if create {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(root)?;
        }
        match fs::symlink_metadata(root) {
            Ok(metadata) => {
                if !metadata.is_dir()
                    || metadata.uid() != rustix::process::geteuid().as_raw()
                    || metadata.mode() & 0o077 != 0
                {
                    return Err(Error::PrivateState);
                }
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}
