//! Durable host preparation; allocation never establishes native readiness or restart permission.

pub mod backend;

#[cfg(test)]
mod test;

use super::native::{bootstrap, lease};
use crate::deployment;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use wrix_core::nix_store::pool::Key;

const HEADROOM: u64 = 1024 * 1024 * 1024;
const RECORD: &str = "reservation.json";

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// pool preparation I/O failed: {0}
    Io(#[from] io::Error),
    /// pool preparation metadata is invalid: {0}
    Json(#[from] serde_json::Error),
    /// host pool preparation is unavailable: {0}
    Host(#[from] wrix_state::host::Error),
    /// host pool paths are invalid: {0}
    Layout(#[from] deployment::Error),
    /// retained pool preparation requires explicit reconciliation; no owner was started
    Retained,
    /// pool preparation ownership or storage is invalid
    Invalid,
    /// pool preparation requires eight GiB of allocatable storage and one GiB of free headroom
    Space,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Reserved,
    Prepared,
}

impl Phase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Prepared => "prepared",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    deployment: deployment::Id,
    pool: Key,
    phase: Phase,
}

impl Record {
    fn read(root: &Path) -> Result<Self, Error> {
        check_directory(root)?;
        let file = private_file(root, RECORD, false)?;
        if file.metadata()?.len() > 4096 {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(Error::Invalid);
        }
        let record: Self = serde_json::from_slice(&bytes)?;
        if record.version != 1 {
            return Err(Error::Invalid);
        }
        Ok(record)
    }

    fn publish(&self, root: &Path) -> Result<(), Error> {
        let mut temporary = tempfile::NamedTempFile::new_in(root)?;
        temporary.write_all(&serde_json::to_vec(self)?)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(root.join(RECORD))
            .map_err(|error| error.error)?;
        File::open(root)?.sync_all()?;
        Ok(())
    }
}

pub(crate) fn inspect(
    host: &wrix_state::host::Host,
    repository: &wrix_core::nix_store::pool::Repository,
    deployment: Option<&deployment::Id>,
) -> Result<Vec<(Key, Phase)>, Error> {
    let mut root = host.roots().state().to_path_buf();
    check_directory(&root)?;
    for part in ["control", "nix", &repository.to_string()] {
        root.push(part);
        match fs::symlink_metadata(&root) {
            Ok(_) => check_parent(&root)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        }
    }
    let systems = [
        wrix_core::nix_store::pool::System::X86_64Linux,
        wrix_core::nix_store::pool::System::Aarch64Linux,
    ];
    for entry in fs::read_dir(&root)? {
        let name = entry?.file_name();
        if !systems.iter().any(|system| name == system.as_str()) {
            tracing::warn!(entry = ?name, "pool repository contains an unknown system entry");
            return Err(Error::Invalid);
        }
    }
    let mut records = Vec::new();
    for system in systems {
        let path = root.join(system.as_str());
        match fs::symlink_metadata(&path) {
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        let record = Record::read(&path)?;
        let expected = Key {
            repository: repository.clone(),
            system,
        };
        if record.pool != expected || deployment != Some(&record.deployment) {
            tracing::warn!(expected_repository = %expected.repository, system = expected.system.as_str(), recorded_repository = %record.pool.repository, expected_deployment = ?deployment, recorded_deployment = ?record.deployment, "pool reservation identity disagrees with inspection scope");
            return Err(Error::Invalid);
        }
        records.push((record.pool, record.phase));
    }
    Ok(records)
}

struct Paths {
    control: PathBuf,
    data: PathBuf,
}

impl Paths {
    fn new(layout: &deployment::Layout, pool: &Key) -> Self {
        Self {
            control: layout
                .control_root()
                .join("nix")
                .join(pool.repository.to_string())
                .join(pool.system.as_str()),
            data: layout.nix_pool(pool),
        }
    }
}

/// Structural parents contain only scoped directories; secret-bearing pool/control directories remain 0700.
pub(crate) fn check_parent(root: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir()
        || metadata.mode() & 0o022 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || root.canonicalize()? != root
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn check_directory(root: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir()
        || metadata.mode() & 0o777 != 0o700
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || root.canonicalize()? != root
    {
        tracing::warn!(path = %root.display(), mode = metadata.mode() & 0o7777, uid = metadata.uid(), "pool directory does not satisfy private canonical ownership");
        return Err(Error::Invalid);
    }
    Ok(())
}

fn private_file(root: &Path, name: &str, create: bool) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create_new(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(name))?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.nlink() != 1
    {
        return Err(Error::Invalid);
    }
    Ok(file)
}

fn create_directory(root: &Path) -> Result<(), Error> {
    let parent = root.parent().ok_or(Error::Invalid)?;
    wrix_core::private_directory::prepare_with_parents(parent).map_err(io::Error::other)?;
    match DirBuilder::new().mode(0o700).create(root) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(Error::Retained),
        Err(error) => return Err(error.into()),
    }
    check_directory(root)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Single preparation attempt; dropping it retains the reservation and any partial allocation.
pub struct Reservation {
    paths: Paths,
    record: Record,
    _ownership: File,
}

impl Reservation {
    fn create(
        layout: &deployment::Layout,
        deployment: &deployment::Id,
        pool: &Key,
    ) -> Result<Self, Error> {
        let paths = Paths::new(layout, pool);
        create_directory(&paths.control)?;
        let ownership = private_file(&paths.control, "ownership", true)?;
        FileExt::try_lock_exclusive(&ownership)?;
        ownership.sync_all()?;
        let record = Record {
            version: 1,
            deployment: deployment.clone(),
            pool: pool.clone(),
            phase: Phase::Reserved,
        };
        record.publish(&paths.control)?;
        create_directory(&paths.data)?;
        Ok(Self {
            paths,
            record,
            _ownership: ownership,
        })
    }

    pub(crate) fn reserve(
        host: &wrix_state::host::Host,
        deployment: &deployment::Id,
        pool: &Key,
    ) -> Result<Self, Error> {
        let state = host.roots().state().parent().ok_or(Error::Invalid)?;
        let cache = host.roots().cache().parent().ok_or(Error::Invalid)?;
        Self::create(&deployment::Layout::new(state, cache)?, deployment, pool)
    }

    pub const fn pool(&self) -> &Key {
        &self.record.pool
    }

    /// Native preallocation, not sparse sizing; no guest or external work is started here.
    pub fn allocate(self, admin_key: lease::Key) -> Result<Prepared, Error> {
        check_directory(&self.paths.control)?;
        check_directory(&self.paths.data)?;
        if Record::read(&self.paths.control)? != self.record {
            return Err(Error::Invalid);
        }
        if let Some(entry) = fs::read_dir(&self.paths.data)?.next() {
            entry?;
            return Err(Error::Retained);
        }
        if fs2::available_space(&self.paths.data)? < bootstrap::CAPACITY + HEADROOM {
            return Err(Error::Space);
        }
        let disk = private_file(&self.paths.data, "store.ext4", true)?;
        FileExt::allocate(&disk, bootstrap::CAPACITY)?;
        if disk.metadata()?.len() != bootstrap::CAPACITY
            || FileExt::allocated_size(&disk)? < bootstrap::CAPACITY
        {
            return Err(Error::Space);
        }
        disk.sync_all()?;
        let mut configuration = private_file(&self.paths.data, "bootstrap.json", true)?;
        configuration.write_all(&serde_json::to_vec(
            &bootstrap::Configuration::preallocated(admin_key),
        )?)?;
        configuration.sync_all()?;
        File::open(&self.paths.data)?.sync_all()?;
        let mut reservation = self;
        reservation.record.phase = Phase::Prepared;
        reservation.record.publish(&reservation.paths.control)?;
        Ok(Prepared { reservation })
    }
}

/// Prepared service data only; this capability grants no worker mount, endpoint or native admission.
pub struct Prepared {
    reservation: Reservation,
}

impl Prepared {
    pub const fn pool(&self) -> &Key {
        self.reservation.pool()
    }

    pub fn data_root(&self) -> &Path {
        &self.reservation.paths.data
    }
}
