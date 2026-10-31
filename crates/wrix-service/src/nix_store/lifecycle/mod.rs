//! Whole-host pool maintenance accounts for every provisioned pool; only explicit update selects new software.
pub(crate) mod gate;
#[cfg(test)]
mod test;
use crate::{
    control::supervision::Package,
    deployment,
    nix_store::owner::{
        self,
        backend::{self, MaintenanceState, Owner, tools::Tools},
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Intent {
    #[default]
    Repair,
    Shutdown,
    Update {
        candidate: crate::nix_store::image::Candidate,
        tools: Tools,
    },
}
impl Intent {
    const fn is_repair(&self) -> bool {
        matches!(self, Self::Repair)
    }
}

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
    /// interrupted installed-store maintenance fences new admissions; repeat the recorded host operation with the same host package
    Pending,
    /// host services are explicitly stopped; ordinary service start must resume the recorded deployment before installed-store admission
    Stopped,
    /// a pending service update fences new admission; inspect `wrix service --host`, then retry `wrix service update` with its recorded exact package or use `wrix service update --cancel` only for a provably unstarted update
    UpdatePending,
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
    fn capture(pool: &Key, state: MaintenanceState, intent: &Intent) -> Result<Self, Error> {
        match state {
            MaintenanceState::Released if intent.is_repair() => Ok(Self::Released(pool.clone())),
            MaintenanceState::Serving | MaintenanceState::Released => {
                Ok(Self::Serving(pool.clone()))
            }
            MaintenanceState::Stopping => Err(Error::Invalid),
        }
    }

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
    #[serde(default, skip_serializing_if = "Intent::is_repair")]
    intent: Intent,
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
        intent: &Intent,
        deployment: Option<&deployment::Id>,
        pools: &[Key],
    ) -> Result<(), Error> {
        self.check_intent(target, intent)?;
        let generation = Store::inspect(host)?.ok_or(Error::Invalid)?.generation()?;
        if self.generation != generation.as_bytes().as_slice()
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

    fn check_intent(&self, target: &Package, intent: &Intent) -> Result<(), Error> {
        if self.target != *target || self.intent != *intent {
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

/// Passive inventory only; absence of pool tools never authorizes omitting a provisioned owner.
pub(crate) fn needs_binding(host: &Host) -> Result<bool, Error> {
    let deployment = crate::control::existing_deployment(host).map_err(io::Error::other)?;
    Ok(Marker::read(host)?.is_some() || !census(host, deployment.as_ref())?.is_empty())
}

/// Pool portion of an explicit whole-deployment update, not a separate pool selector.
///
/// Begin before changing core roles; complete only after compatible owned core-role readiness.
/// Drop preserves intent and fences admission. Candidate failure never silently resumes the old software.
pub struct Update<'a>(Repair<'a>);
impl<'a> Update<'a> {
    /// An unprovisioned deployment still retains census exclusion through core activation.
    pub fn unprovisioned(host: &'a Host) -> Result<Self, Error> {
        let guard = gate::exclusive(host)?;
        let deployment = crate::control::existing_deployment(host).map_err(io::Error::other)?;
        if deployment.is_none() || needs_binding(host)? {
            return Err(Error::Conflict);
        }
        Ok(Self(Repair {
            host,
            marker: None,
            _gate: guard,
        }))
    }

    pub fn begin(
        host: &'a Host,
        target: &Package,
        candidate: &crate::nix_store::image::Candidate,
        tools: &Tools,
    ) -> Result<Self, Error> {
        if !tools.belongs_to(target) {
            return Err(Error::Conflict);
        }
        Repair::begin_intent(
            host,
            target,
            Intent::Update {
                candidate: candidate.clone(),
                tools: tools.clone(),
            },
        )
        .map(Self)
    }

    pub fn complete(self) -> Result<(), Error> {
        self.0.complete()
    }
}

/// Pool shutdown retains all native data and roots. Busy or uncertain readers cannot be forced out.
pub struct Shutdown<'a>(ShutdownState<'a>);

enum ShutdownState<'a> {
    Transaction(Repair<'a>),
    Retained {
        host: &'a Host,
        marker: Marker,
        _gate: gate::Guard,
    },
}

impl<'a> Shutdown<'a> {
    pub fn begin(host: &'a Host, target: &Package) -> Result<Self, Error> {
        let guard = gate::exclusive(host)?;
        if let Some(marker) = Marker::read(host)?
            && marker.intent != Intent::Shutdown
        {
            let deployment = crate::control::existing_deployment(host).map_err(io::Error::other)?;
            let pools = census(host, deployment.as_ref())?;
            marker.check(
                host,
                &marker.target,
                &marker.intent,
                deployment.as_ref(),
                &pools,
            )?;
            let mut owners = owners(host, &marker.deployment, &pools, true)?;
            for (owner, state) in &mut owners {
                if *state != MaintenanceState::Released {
                    owner.shutdown()?;
                }
            }
            drop(owners);
            return Ok(Self(ShutdownState::Retained {
                host,
                marker,
                _gate: guard,
            }));
        }
        drop(guard);
        Repair::begin_intent(host, target, Intent::Shutdown)
            .map(|operation| Self(ShutdownState::Transaction(operation)))
    }

    /// Call after core/controller shutdown. Interrupted updates retain their exact intent.
    pub fn complete(self) -> Result<(), Error> {
        match self.0 {
            ShutdownState::Transaction(operation) => operation.complete(),
            ShutdownState::Retained { host, marker, .. } => {
                if Marker::read(host)?.as_ref() != Some(&marker) {
                    return Err(Error::Conflict);
                }
                let pools = census(host, Some(&marker.deployment))?;
                marker.check(
                    host,
                    &marker.target,
                    &marker.intent,
                    Some(&marker.deployment),
                    &pools,
                )?;
                if owners(host, &marker.deployment, &pools, true)?
                    .iter()
                    .any(|(_, state)| *state != MaintenanceState::Released)
                {
                    return Err(Error::Conflict);
                }
                Ok(())
            }
        }
    }
}

/// Explicit host operation only. Drop preserves an interruption marker and never rolls back or resumes native work.
pub struct Repair<'a> {
    host: &'a Host,
    marker: Option<Marker>,
    _gate: gate::Guard,
}
impl<'a> Repair<'a> {
    pub fn begin(host: &'a Host, target: &Package) -> Result<Self, Error> {
        Self::begin_intent(host, target, Intent::Repair)
    }

    fn begin_intent(host: &'a Host, target: &Package, intent: Intent) -> Result<Self, Error> {
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
            marker.check(host, target, &intent, Some(&deployment), &pools)?;
        }
        let mut owners = owners(host, &deployment, &pools, pending.is_some())?;
        if let Intent::Update { tools, .. } = &intent {
            // Check every target runtime before releasing any peer; this does not realize a candidate.
            for pool in &pools {
                tools.check(pool.system)?;
            }
        }
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
                .map(|(owner, state)| Entry::capture(owner.pool(), *state, &intent))
                .collect::<Result<_, _>>()?;
            let marker = Marker {
                version: 1,
                generation,
                deployment,
                target: target.clone(),
                intent,
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
    /// Call only after compatible owned controller readiness. Repair resumes recorded software;
    /// the explicit update wrapper alone carries authority to select the recorded candidate.
    pub fn complete(self) -> Result<(), Error> {
        let Some(marker) = &self.marker else {
            return Ok(());
        };
        let deployment =
            crate::control::existing_deployment(self.host).map_err(io::Error::other)?;
        let pools = census(self.host, deployment.as_ref())?;
        marker.check(
            self.host,
            &marker.target,
            &marker.intent,
            deployment.as_ref(),
            &pools,
        )?;
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
                match &marker.intent {
                    Intent::Repair => owner.resume()?,
                    Intent::Shutdown => continue,
                    Intent::Update { candidate, tools } => owner.update(candidate, tools)?,
                }
                owner.maintenance_state(false)?;
            } else if let Intent::Update { candidate, tools } = &marker.intent
                && !owner.selected(candidate, tools)?
            {
                return Err(Error::Conflict);
            }
        }
        fs::remove_file(self.host.roots().state().join(gate::MARKER))?;
        File::open(self.host.roots().state())?.sync_all()?;
        Ok(())
    }
}
