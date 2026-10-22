//! Durable namespace policy, independent of runtime generations and live traffic grants.

#[cfg(test)]
mod test;

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    ops::{Deref, DerefMut},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::{Error, MAX_FRAME, MAX_NAMESPACES, Namespace, Policy, VERSION};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u16,
    entries: Vec<Entry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    namespace: Namespace,
    policy: Policy,
}

/// Original v1 kernel ownership fence. New registry semantics may be installed
/// only while an old cached controller is positively excluded by this inode.
pub(super) struct Ownership {
    file: File,
}
impl Ownership {
    pub(super) fn acquire(root: &Path) -> Result<Self, Error> {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join("ownership"))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != fs::metadata(root)?.uid()
        {
            return Err(Error::PrivateRoot);
        }
        file.try_lock_exclusive()?;
        Ok(Self { file })
    }
}
impl Drop for Ownership {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            tracing::warn!(%error, "could not release controller ownership");
        }
    }
}

pub(super) struct Lease {
    file: File,
}

pub(super) struct Guard {
    file: File,
    store: Store,
    namespaces: BTreeMap<Namespace, Policy>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            tracing::warn!(%error, "could not release namespace policy lease");
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            tracing::warn!(%error, "could not release registry metadata lock");
        }
    }
}

impl Deref for Guard {
    type Target = BTreeMap<Namespace, Policy>;
    fn deref(&self) -> &Self::Target {
        &self.namespaces
    }
}
impl DerefMut for Guard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.namespaces
    }
}
impl Guard {
    pub(super) fn commit(&self) -> Result<(), Error> {
        self.store.commit(&self.namespaces)
    }
}

pub(super) struct Store {
    path: PathBuf,
    uid: u32,
}

impl Store {
    pub(super) fn open(root: &Path) -> Result<Self, Error> {
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.is_dir() || metadata.mode() & 0o077 != 0 || root.canonicalize()? != root {
            return Err(Error::PrivateRoot);
        }
        Ok(Self {
            path: root.join("namespaces.json"),
            uid: fs::metadata(root)?.uid(),
        })
    }

    pub(super) fn lease(&self, namespace: &Namespace) -> Result<Lease, Error> {
        Ok(Lease {
            file: self.lock_file(&format!("namespace-{}.lock", namespace.as_str()), false)?,
        })
    }

    /// Cross-process authority lock: controller IPC and trusted host lifecycle
    /// share one registry; no cached policy can resurrect a retired SQL grant.
    pub(super) fn lock(&self) -> Result<Guard, Error> {
        let root = self.path.parent().ok_or(Error::PrivateRoot)?;
        let file = self.lock_file("namespaces.lock", true)?;
        if !self.path.try_exists()? {
            let _owner = Ownership::acquire(root).map_err(|error| match error {
                Error::Io(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    Error::RegistryUpgrade
                }
                other => other,
            })?;
            self.commit(&BTreeMap::new())?;
        }
        Ok(Guard {
            file,
            store: Self::open(root)?,
            namespaces: self.load()?,
        })
    }

    fn lock_file(&self, name: &str, wait: bool) -> Result<File, Error> {
        let root = self.path.parent().ok_or(Error::PrivateRoot)?;
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join(name))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != self.uid
            || metadata.mode() & 0o077 != 0
        {
            return Err(Error::PrivateRoot);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => break,
                Err(error)
                    if wait
                        && error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(file)
    }

    /// Explicit startup/recovery migration, preserving policy under original
    /// controller exclusion. An old binary rejects the new snapshot on restart.
    pub(super) fn initialize(&self, _owner: &Ownership) -> Result<(), Error> {
        let _metadata = self.lock_file("namespaces.lock", true)?;
        match self.load() {
            Ok(_) => Ok(()),
            Err(Error::RegistryUpgrade) => self.commit(&self.load_version(true)?),
            Err(error) => Err(error),
        }
    }

    pub(super) fn load(&self) -> Result<BTreeMap<Namespace, Policy>, Error> {
        self.load_version(false)
    }

    fn load_version(&self, legacy: bool) -> Result<BTreeMap<Namespace, Policy>, Error> {
        let file = match File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return if legacy {
                    Ok(BTreeMap::new())
                } else {
                    Err(Error::RegistryUpgrade)
                };
            }
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != self.uid
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(Error::PrivateRoot);
        }
        let mut bytes = Vec::new();
        file.take(u64::try_from(MAX_FRAME).map_err(|_| Error::FrameLimit)? + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FRAME {
            return Err(Error::FrameLimit);
        }
        let snapshot: Snapshot = serde_json::from_slice(&bytes)?;
        if snapshot.version == 1 && !legacy {
            return Err(Error::RegistryUpgrade);
        }
        if (snapshot.version != VERSION && !(legacy && snapshot.version == 1))
            || snapshot.entries.len() > MAX_NAMESPACES
        {
            return Err(Error::InvalidRegistration);
        }
        let mut namespaces = BTreeMap::new();
        for entry in snapshot.entries {
            if namespaces.insert(entry.namespace, entry.policy).is_some() {
                return Err(Error::InvalidRegistration);
            }
        }
        Ok(namespaces)
    }

    /// Commit before exposing policy to issuers; failed writes preserve the previous registry.
    pub(super) fn commit(&self, namespaces: &BTreeMap<Namespace, Policy>) -> Result<(), Error> {
        let snapshot = Snapshot {
            version: VERSION,
            entries: namespaces
                .iter()
                .map(|(namespace, policy)| Entry {
                    namespace: namespace.clone(),
                    policy: *policy,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&snapshot)?;
        if bytes.len() > MAX_FRAME || namespaces.len() > MAX_NAMESPACES {
            return Err(Error::FrameLimit);
        }
        let root = self.path.parent().ok_or(Error::PrivateRoot)?;
        let mut file = tempfile::NamedTempFile::new_in(root)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(&self.path).map_err(|error| error.error)?;
        File::open(root)?.sync_all()?;
        Ok(())
    }
}
