//! Deployment role leases are private controller state, never namespace reservations.

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    num::NonZeroU16,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::Error;
use crate::route::{Listeners, Role};

const MAX_BYTES: u64 = 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u16,
    ports: [NonZeroU16; 4],
}

pub(super) struct Store {
    path: PathBuf,
    uid: u32,
    issued: Mutex<bool>,
}

impl Store {
    pub(super) fn open(root: &Path) -> Result<Self, Error> {
        let store = Self {
            path: root.join("role-endpoints.json"),
            uid: fs::metadata(root)?.uid(),
            issued: Mutex::new(false),
        };
        store.load()?;
        Ok(store)
    }

    fn load(&self) -> Result<[Option<NonZeroU16>; 4], Error> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok([None; 4]),
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() || metadata.uid() != self.uid || metadata.mode() & 0o077 != 0 {
            return Err(Error::PrivateRoot);
        }
        let mut bytes = Vec::new();
        File::open(&self.path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Error::FrameLimit);
        }
        let snapshot: Snapshot = serde_json::from_slice(&bytes)?;
        if snapshot.version != 1
            || !(23_000..25_000).contains(&snapshot.ports[0].get())
            || !(21_000..23_000).contains(&snapshot.ports[1].get())
            || snapshot
                .ports
                .iter()
                .enumerate()
                .any(|(index, port)| snapshot.ports[..index].contains(port))
        {
            return Err(Error::InvalidEndpoints);
        }
        Ok(snapshot.ports.map(Some))
    }

    pub(super) fn bound_ports(&self) -> Result<Option<[NonZeroU16; 4]>, Error> {
        let issued = self
            .issued
            .try_lock()
            .map_err(|_| Error::ControllerReadiness)?;
        if !*issued {
            return Ok(None);
        }
        let [sql, cache, egress, compiler] = self.load()?;
        Ok(Some([
            sql.ok_or(Error::InvalidEndpoints)?,
            cache.ok_or(Error::InvalidEndpoints)?,
            egress.ok_or(Error::InvalidEndpoints)?,
            compiler.ok_or(Error::InvalidEndpoints)?,
        ]))
    }

    pub(super) async fn bind(&self) -> Result<Listeners, Error> {
        let mut issued = self.issued.lock().await;
        if *issued {
            return Err(Error::EndpointsAlreadyBound);
        }
        let listeners = Listeners::bind_deployment(self.load()?).await?;
        let mut ports = Vec::with_capacity(4);
        for role in Role::ALL {
            ports.push(
                NonZeroU16::new(listeners.address(role)?.port()).ok_or(Error::InvalidEndpoints)?,
            );
        }
        let snapshot = Snapshot {
            version: 1,
            ports: ports.try_into().map_err(|_| Error::InvalidEndpoints)?,
        };
        let root = self.path.parent().ok_or(Error::PrivateRoot)?;
        let mut file = tempfile::NamedTempFile::new_in(root)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&serde_json::to_vec(&snapshot)?)?;
        file.as_file().sync_all()?;
        file.persist(&self.path).map_err(|error| error.error)?;
        *issued = true;
        Ok(listeners)
    }
}
