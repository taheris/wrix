//! Durable namespace policy, independent of runtime generations and live traffic grants.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

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

pub(super) struct Store {
    path: PathBuf,
    uid: u32,
}

impl Store {
    pub(super) fn open(root: &Path) -> Result<Self, Error> {
        Ok(Self {
            path: root.join("namespaces.json"),
            uid: fs::metadata(root)?.uid(),
        })
    }

    pub(super) fn load(&self) -> Result<BTreeMap<Namespace, Policy>, Error> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() || metadata.uid() != self.uid || metadata.mode() & 0o077 != 0 {
            return Err(Error::PrivateRoot);
        }
        let mut bytes = Vec::new();
        File::open(&self.path)?
            .take(u64::try_from(MAX_FRAME).map_err(|_| Error::FrameLimit)? + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FRAME {
            return Err(Error::FrameLimit);
        }
        let snapshot: Snapshot = serde_json::from_slice(&bytes)?;
        if snapshot.version != VERSION || snapshot.entries.len() > MAX_NAMESPACES {
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
        Ok(())
    }
}
