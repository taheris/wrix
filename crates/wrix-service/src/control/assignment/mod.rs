//! Selected generation intent is serialized by the controller, not by workspace locks.

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    num::NonZeroU64,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use wrix_sandbox::image::{Digest, ImageRef};

use super::Error;
use crate::deployment::{
    Generation, Id,
    selection::{Record, Target},
};

const MAX_BYTES: u64 = 4096;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u16,
    selection: Record,
}

pub(super) struct Store {
    path: PathBuf,
    current: Mutex<Option<Record>>,
}

impl Store {
    pub(super) fn open(root: &Path, deployment: Option<&Id>) -> Result<Self, Error> {
        let path = root.join("selection.json");
        let current = load(root, &path)?;
        if current
            .as_ref()
            .is_some_and(|record| Some(record.deployment()) != deployment)
        {
            return Err(Error::InvalidSelection);
        }
        Ok(Self {
            path,
            current: Mutex::new(current),
        })
    }

    pub(super) fn inspect(&self) -> Result<Option<Record>, Error> {
        Ok(self
            .current
            .lock()
            .map_err(|_| Error::LockPoisoned)?
            .clone())
    }

    /// Ordinary bootstrap cannot replace an existing selection, including stopped/uncertain roles.
    pub(super) fn bootstrap(
        &self,
        deployment: &Id,
        image: ImageRef,
        digest: Digest,
    ) -> Result<Record, Error> {
        let mut current = self.current.lock().map_err(|_| Error::LockPoisoned)?;
        if let Some(record) = current.as_ref() {
            return Ok(record.clone());
        }
        let target = Target::new(Generation::new(NonZeroU64::MIN), image, digest);
        let record = Record::initial(deployment.clone(), target);
        let bytes = serde_json::to_vec(&Snapshot {
            version: 1,
            selection: record.clone(),
        })?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Error::FrameLimit);
        }
        let root = self.path.parent().ok_or(Error::PrivateRoot)?;
        let mut file = tempfile::NamedTempFile::new_in(root)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(&self.path)
            .map_err(|error| error.error)?;
        *current = Some(record.clone());
        Ok(record)
    }
}

fn load(root: &Path, path: &Path) -> Result<Option<Record>, Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file()
        || metadata.uid() != fs::metadata(root)?.uid()
        || metadata.mode() & 0o077 != 0
    {
        return Err(Error::PrivateRoot);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::FrameLimit);
    }
    let snapshot: Snapshot = serde_json::from_slice(&bytes)?;
    if snapshot.version != 1 {
        return Err(Error::InvalidSelection);
    }
    Ok(Some(snapshot.selection))
}
