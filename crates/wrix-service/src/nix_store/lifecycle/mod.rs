//! Host controller repair accounts for every provisioned pool and never changes recorded pool software implicitly.
pub(crate) mod gate;
use crate::{
    control::supervision::Package,
    deployment,
    nix_store::owner::{
        self,
        backend::{self, MaintenanceState, Owner},
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
};
use wrix_core::nix_store::pool::{Key, Repository};
use wrix_state::{host::Host, store::Store};

const LIMIT: usize = 256;
const DOCUMENT: u64 = 65_536;
#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// installed-store maintenance I/O failed; interrupted ownership is retained: {0}
    Io(#[from] io::Error),
    /// installed-store maintenance state failed: {0}
    State(#[from] wrix_state::store::Error),
    /// installed-store maintenance metadata failed: {0}
    Json(#[from] serde_json::Error),
    /// installed-store inventory is unavailable: {0}
    Inventory(#[from] owner::Error),
    /// installed-store native maintenance failed: {0}
    Backend(#[from] backend::Error),
    /// installed-store maintenance metadata or ownership is inconsistent; protection is retained
    Invalid,
    /// installed-store preparation, native work or a manager is busy; drain it before controller repair
    Busy,
    /// interrupted installed-store maintenance fences new admissions; rerun service repair with the same host package
    Pending,
    /// installed-store maintenance belongs to a different host package or state generation
    Conflict,
    /// installed-store maintenance exceeds its bounded pool inventory
    Capacity,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "original",
    content = "pool",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Entry {
    Serving(Key),
    Released(Key),
}
impl Entry {
    const fn pool(&self) -> &Key {
        match self {
            Self::Serving(pool) | Self::Released(pool) => pool,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u16,
    generation: Vec<u8>,
    deployment: deployment::Id,
    target: Package,
    entries: Vec<Entry>,
}
impl Marker {
    fn read(host: &Host) -> Result<Option<Self>, Error> {
        let path = host.roots().state().join(gate::MARKER);
        let file = match gate::private_file(&path) {
            Ok(file) => file,
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > DOCUMENT {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::new();
        file.take(DOCUMENT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > DOCUMENT {
            return Err(Error::Invalid);
        }
        let marker: Self = serde_json::from_slice(&bytes)?;
        if marker.version != 1
            || marker.generation.len() != 32
            || marker.entries.is_empty()
            || marker.entries.len() > LIMIT
        {
            return Err(Error::Invalid);
        }
        for (index, entry) in marker.entries.iter().enumerate() {
            if marker.entries[..index]
                .iter()
                .any(|prior| prior.pool() == entry.pool())
            {
                return Err(Error::Invalid);
            }
        }
        Ok(Some(marker))
    }
    fn publish(&self, host: &Host) -> Result<(), Error> {
        let root = host.roots().state();
        let mut file = tempfile::NamedTempFile::new_in(root)?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() as u64 > DOCUMENT {
            return Err(Error::Invalid);
        }
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(root.join(gate::MARKER))
            .map_err(|error| error.error)?;
        File::open(root)?.sync_all()?;
        Ok(())
    }
    fn check(
        &self,
        host: &Host,
        target: &Package,
        deployment: Option<&deployment::Id>,
        pools: &[Key],
    ) -> Result<(), Error> {
        let generation = Store::inspect(host)?.ok_or(Error::Invalid)?.generation()?;
        if self.target != *target
            || self.generation != generation.as_bytes().as_slice()
            || deployment != Some(&self.deployment)
        {
            return Err(Error::Conflict);
        }
        if self.entries.len() != pools.len()
            || self
                .entries
                .iter()
                .zip(pools)
                .any(|(entry, pool)| entry.pool() != pool)
        {
            return Err(Error::Conflict);
        }
        Ok(())
    }
}
fn census(host: &Host, deployment: Option<&deployment::Id>) -> Result<Vec<Key>, Error> {
    let root = host.roots().state().join("control/nix");
    match fs::symlink_metadata(&root) {
        Ok(_) => owner::check_parent(&root)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    }
    let mut pools = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let repository = Repository::try_from(
            entry
                .file_name()
                .into_string()
                .map_err(|_| Error::Invalid)?,
        )
        .map_err(|_| Error::Invalid)?;
        for (pool, _) in owner::inspect(host, &repository, deployment)? {
            pools.push(pool);
            if pools.len() > LIMIT {
                return Err(Error::Capacity);
            }
        }
    }
    pools.sort_by_cached_key(|pool| (pool.repository.to_string(), pool.system.as_str()));
    Ok(pools)
}
fn owners(
    host: &Host,
    deployment: &deployment::Id,
    pools: &[Key],
    recovering: bool,
) -> Result<Vec<(Owner, MaintenanceState)>, Error> {
    // Acquire and validate every manager before any stop; a bad peer cannot cause a partial preflight stop.
    let mut owners = Vec::new();
    for pool in pools {
        let owner = Owner::open(host, deployment, pool)?;
        let state = owner.maintenance_state(recovering)?;
        owners.push((owner, state));
    }
    Ok(owners)
}

pub(crate) fn unmanaged_repair(host: &Host) -> Result<gate::Guard, Error> {
    let guard = gate::exclusive(host)?;
    gate::check_clear(host)?;
    let deployment = crate::control::existing_deployment(host).map_err(io::Error::other)?;
    if !census(host, deployment.as_ref())?.is_empty() {
        return Err(Error::Conflict);
    }
    Ok(guard)
}

/// Explicit host operation only. Drop preserves an interruption marker and never rolls back or resumes native work.
pub struct Repair<'a> {
    host: &'a Host,
    marker: Option<Marker>,
    _gate: gate::Guard,
}
impl<'a> Repair<'a> {
    pub fn begin(host: &'a Host, target: &Package) -> Result<Self, Error> {
        let guard = gate::exclusive(host)?;
        let deployment = crate::control::existing_deployment(host).map_err(io::Error::other)?;
        let pools = census(host, deployment.as_ref())?;
        let pending = Marker::read(host)?;
        if pools.is_empty() && pending.is_none() {
            return Ok(Self {
                host,
                marker: None,
                _gate: guard,
            });
        }
        let deployment = deployment.ok_or(Error::Invalid)?;
        if let Some(marker) = &pending {
            marker.check(host, target, Some(&deployment), &pools)?;
        }
        let mut owners = owners(host, &deployment, &pools, pending.is_some())?;
        let marker = if let Some(marker) = pending {
            marker
        } else {
            let generation = Store::inspect(host)?
                .ok_or(Error::Invalid)?
                .generation()?
                .as_bytes()
                .to_vec();
            let entries = owners
                .iter()
                .map(|(owner, state)| match state {
                    MaintenanceState::Serving => Ok(Entry::Serving(owner.pool().clone())),
                    MaintenanceState::Released => Ok(Entry::Released(owner.pool().clone())),
                    MaintenanceState::Stopping => Err(Error::Invalid),
                })
                .collect::<Result<_, _>>()?;
            let marker = Marker {
                version: 1,
                generation,
                deployment,
                target: target.clone(),
                entries,
            };
            marker.publish(host)?;
            marker
        };
        for (entry, (_, state)) in marker.entries.iter().zip(&owners) {
            if matches!(entry, Entry::Released(_)) && *state != MaintenanceState::Released {
                return Err(Error::Conflict);
            }
        }
        for (entry, (owner, state)) in marker.entries.iter().zip(&mut owners) {
            if matches!(entry, Entry::Serving(_)) && *state != MaintenanceState::Released {
                owner.shutdown()?;
            }
        }
        // No backend lock survives into package realization or controller startup; durable admission fencing does.
        drop(owners);
        Ok(Self {
            host,
            marker: Some(marker),
            _gate: guard,
        })
    }
    /// Call only after compatible owned controller readiness. Resume recorded software, never the candidate pool image.
    pub fn complete(self) -> Result<(), Error> {
        let Some(marker) = &self.marker else {
            return Ok(());
        };
        let deployment =
            crate::control::existing_deployment(self.host).map_err(io::Error::other)?;
        let pools = census(self.host, deployment.as_ref())?;
        marker.check(self.host, &marker.target, deployment.as_ref(), &pools)?;
        if Marker::read(self.host)?.as_ref() != Some(marker) {
            return Err(Error::Conflict);
        }
        let mut owners = owners(self.host, &marker.deployment, &pools, true)?;
        for (entry, (_, state)) in marker.entries.iter().zip(&owners) {
            if *state == MaintenanceState::Stopping
                || (matches!(entry, Entry::Released(_)) && *state != MaintenanceState::Released)
            {
                return Err(Error::Conflict);
            }
        }
        for (entry, (owner, state)) in marker.entries.iter().zip(&mut owners) {
            if matches!(entry, Entry::Serving(_)) && *state == MaintenanceState::Released {
                owner.resume()?;
                owner.maintenance_state(false)?;
            }
        }
        fs::remove_file(self.host.roots().state().join(gate::MARKER))?;
        File::open(self.host.roots().state())?.sync_all()?;
        Ok(())
    }
}
