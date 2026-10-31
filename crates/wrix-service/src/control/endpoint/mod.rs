//! Deployment role leases are private controller state, never namespace reservations.

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    net::Ipv4Addr,
    num::NonZeroU16,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::Error;
use crate::route::{Listeners, Role};

#[cfg(test)]
mod test;

const MAX_BYTES: u64 = 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u16,
    ports: Vec<NonZeroU16>,
}

pub(super) struct Store {
    path: PathBuf,
    uid: u32,
    issued: Mutex<bool>,
    guest: Mutex<Option<Ipv4Addr>>,
}

impl Store {
    fn at(root: &Path) -> Result<Self, Error> {
        Ok(Self {
            path: root.join("role-endpoints.json"),
            uid: fs::metadata(root)?.uid(),
            issued: Mutex::new(false),
            guest: Mutex::new(None),
        })
    }

    pub(super) fn open(root: &Path) -> Result<Self, Error> {
        let store = Self::at(root)?;
        store.load()?;
        Ok(store)
    }

    fn state(&self) -> Result<&Path, Error> {
        self.path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or(Error::PrivateRoot)
    }

    fn read_snapshot(&self) -> Result<[Option<NonZeroU16>; 5], Error> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok([None; 5]),
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file()
            || metadata.uid() != self.uid
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
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
        if !matches!((snapshot.version, snapshot.ports.len()), (1, 4) | (2, 5)) {
            return Err(Error::InvalidEndpoints);
        }
        let mut ports = [None; 5];
        for (slot, port) in ports.iter_mut().zip(snapshot.ports) {
            *slot = Some(port);
        }
        validate(ports)?;
        Ok(ports)
    }

    fn retained(&self) -> Result<[Option<NonZeroU16>; 5], Error> {
        crate::lifecycle::generation::retained_role_ports(self.state()?)
            .map_err(|error| io::Error::other(error).into())
    }

    fn merge(
        mut ports: [Option<NonZeroU16>; 5],
        retained: [Option<NonZeroU16>; 5],
        repair: bool,
    ) -> Result<[Option<NonZeroU16>; 5], Error> {
        for (index, retained) in retained.into_iter().enumerate() {
            if let Some(retained) = retained {
                if !repair
                    && let Some(recorded) = ports[index]
                    && recorded != retained
                {
                    return Err(Error::RetainedEndpoints {
                        role: Role::ALL[index],
                        retained,
                        recorded,
                    });
                }
                ports[index] = Some(retained);
            }
        }
        validate(ports)?;
        Ok(ports)
    }

    fn load(&self) -> Result<[Option<NonZeroU16>; 5], Error> {
        Self::merge(self.read_snapshot()?, self.retained()?, false)
    }

    fn publish(&self, listeners: &Listeners) -> Result<(), Error> {
        let mut ports = Vec::with_capacity(5);
        for role in Role::ALL {
            ports.push(
                NonZeroU16::new(listeners.address(role)?.port()).ok_or(Error::InvalidEndpoints)?,
            );
        }
        let root = self.path.parent().ok_or(Error::PrivateRoot)?;
        let mut file = tempfile::NamedTempFile::new_in(root)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&serde_json::to_vec(&Snapshot { version: 2, ports })?)?;
        file.as_file().sync_all()?;
        file.persist(&self.path).map_err(|error| error.error)?;
        File::open(root)?.sync_all()?;
        Ok(())
    }

    pub(super) fn bound_ports(&self) -> Result<Option<[NonZeroU16; 5]>, Error> {
        let issued = self
            .issued
            .try_lock()
            .map_err(|_| Error::ControllerReadiness)?;
        if !*issued {
            return Ok(None);
        }
        let [sql, cache, egress, compiler, sync] = self.read_snapshot()?;
        Ok(Some([
            sql.ok_or(Error::InvalidEndpoints)?,
            cache.ok_or(Error::InvalidEndpoints)?,
            egress.ok_or(Error::InvalidEndpoints)?,
            compiler.ok_or(Error::InvalidEndpoints)?,
            sync.ok_or(Error::InvalidEndpoints)?,
        ]))
    }

    pub(super) fn guest_address(&self) -> Result<Option<Ipv4Addr>, Error> {
        Ok(*self
            .guest
            .try_lock()
            .map_err(|_| Error::ControllerReadiness)?)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn publish_guest(&self, address: Ipv4Addr) -> Result<(), Error> {
        *self
            .guest
            .try_lock()
            .map_err(|_| Error::ControllerReadiness)? = Some(address);
        Ok(())
    }

    pub(super) async fn bind(&self) -> Result<Listeners, Error> {
        let mut issued = self.issued.lock().await;
        if *issued {
            return Err(Error::EndpointsAlreadyBound);
        }
        let retained = self.retained()?;
        let ports = Self::merge(self.read_snapshot()?, retained, false)?;
        let listeners =
            Listeners::bind_deployment(ports, retained.map(|port| port.is_some())).await?;
        self.publish(&listeners)?;
        *issued = true;
        Ok(listeners)
    }
}

fn validate(ports: [Option<NonZeroU16>; 5]) -> Result<(), Error> {
    for (index, port) in ports.iter().copied().enumerate() {
        if let Some(port) = port {
            let valid_range = match Role::ALL[index] {
                Role::Sql | Role::Sync => (23_000..25_000).contains(&port.get()),
                Role::CacheRead => (21_000..23_000).contains(&port.get()),
                Role::Egress | Role::Compiler => true,
            };
            if !valid_range || ports[..index].contains(&Some(port)) {
                return Err(Error::InvalidEndpoints);
            }
        }
    }
    Ok(())
}

pub fn needs_repair(state: &Path) -> Result<bool, Error> {
    let store = Store::at(&state.join("control/authority"))?;
    let prior = store.read_snapshot()?;
    Ok(Store::merge(prior, store.retained()?, true)? != prior)
}

/// Caller holds explicit stopped-update maintenance and native controller lifecycle exclusion.
pub fn repair(state: &Path, attempt: &str) -> Result<(), Error> {
    if attempt.len() != 32 || !attempt.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::InvalidEndpoints);
    }
    let root = state.join("control/authority");
    let before = fs::symlink_metadata(root.join("ownership"))?;
    if !before.is_file() {
        return Err(Error::PrivateRoot);
    }
    let _owner = super::registration::Ownership::acquire(&root).map_err(|error| match error {
        Error::Io(error) if error.kind() == io::ErrorKind::WouldBlock => {
            Error::EndpointOwnershipBusy
        }
        other => other,
    })?;
    let after = fs::symlink_metadata(root.join("ownership"))?;
    if before.ino() != after.ino() || before.dev() != after.dev() {
        return Err(Error::PrivateRoot);
    }
    let store = Store::at(&root)?;
    let desired = Store::merge(store.read_snapshot()?, store.retained()?, true)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listeners = Listeners::bind_deployment(desired, [true; 5]).await?;
        archive(&root, attempt)?;
        store.publish(&listeners)
    })
}

fn archive(root: &Path, attempt: &str) -> Result<(), Error> {
    use sha2::{Digest, Sha256};
    let Some(bytes) = crate::lifecycle::deployment::document(root, "role-endpoints.json")
        .map_err(io::Error::other)?
    else {
        return Ok(());
    };
    let name = format!(
        "role-endpoints-before-{attempt}-{:x}.json",
        Sha256::digest(&bytes)
    );
    match crate::lifecycle::deployment::document(root, &name).map_err(io::Error::other)? {
        Some(saved) if saved == bytes => (),
        Some(_) => return Err(Error::InvalidEndpoints),
        None => {
            let mut file = tempfile::NamedTempFile::new_in(root)?;
            file.write_all(&bytes)?;
            file.as_file().sync_all()?;
            file.persist_noclobber(root.join(name))
                .map_err(|error| error.error)?;
        }
    }
    File::open(root)?.sync_all()?;
    Ok(())
}
