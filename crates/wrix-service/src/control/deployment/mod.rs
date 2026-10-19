//! Durable deployment identity under the controller's exclusive host ownership.

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

use serde::{Deserialize, Serialize};

use super::Error;
use crate::deployment::Id;

const MAX_BYTES: u64 = 256;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u16,
    id: Id,
}

/// Loading never creates an identity; malformed existing state fails before authority issuance.
pub(super) fn load(root: &Path) -> Result<Option<Id>, Error> {
    let path = root.join("deployment.json");
    let metadata = match fs::symlink_metadata(&path) {
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
        return Err(Error::InvalidDeployment);
    }
    Ok(Some(snapshot.id))
}

/// An identity records deployment membership only, not runtime ownership or native readiness.
pub(super) fn create(root: &Path) -> Result<Id, Error> {
    let mut random = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let value: String = random
        .into_iter()
        .flat_map(|byte| {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 15)]),
            ]
        })
        .collect();
    let id = Id::try_from(value).map_err(|_| Error::InvalidDeployment)?;
    let snapshot = Snapshot {
        version: 1,
        id: id.clone(),
    };
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(&serde_json::to_vec(&snapshot)?)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(root.join("deployment.json"))
        .map_err(|error| error.error)?;
    Ok(id)
}
