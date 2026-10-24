//! Complete exact-image pins are distinct from interrupted imports; filesystem descriptions never repair them.
use super::{Id, StorePath};
use crate::nix_store::native::Error;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Missing,
    Pinned,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Importing,
    Pinned,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    image: Id,
    paths: BTreeSet<StorePath>,
    imports: BTreeSet<StorePath>,
    phase: Phase,
}

pub struct Registry {
    records: PathBuf,
    pins: PathBuf,
}
pub struct Pending {
    registry: Registry,
    record: Record,
}

fn directory(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o700
        || path.canonicalize()? != path
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn create(path: &Path) -> Result<(), Error> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => File::open(path.parent().ok_or(Error::Invalid)?)?.sync_all()?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => directory(path)?,
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
fn absent(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}
fn paths(input: &[StorePath]) -> Result<BTreeSet<StorePath>, Error> {
    let paths: BTreeSet<_> = input.iter().cloned().collect();
    if paths.is_empty() || paths.len() > 4096 || paths.len() != input.len() {
        return Err(Error::Invalid);
    }
    Ok(paths)
}
impl Registry {
    pub fn native() -> Self {
        Self {
            records: "/nix/var/nix/wrix-images".into(),
            pins: "/nix/var/nix/gcroots/images".into(),
        }
    }
    fn read(&self, image: &Id) -> Result<Record, Error> {
        directory(&self.records)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.records.join(image.as_str()))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
            || metadata.len() > 2_097_152
        {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::new();
        file.take(2_097_153).read_to_end(&mut bytes)?;
        if bytes.len() > 2_097_152 {
            return Err(Error::Invalid);
        }
        let record: Record = serde_json::from_slice(&bytes)?;
        if record.version != 1
            || &record.image != image
            || record.paths.is_empty()
            || record.paths.len() > 4096
            || !record.imports.is_subset(&record.paths)
        {
            return Err(Error::Invalid);
        }
        Ok(record)
    }
    pub fn inspect(&self, image: &Id, expected: &[StorePath]) -> Result<Status, Error> {
        let expected = paths(expected)?;
        let records_absent = absent(&self.records)?;
        if !records_absent {
            directory(&self.records)?;
        }
        let pins_absent = absent(&self.pins)?;
        if !pins_absent {
            directory(&self.pins)?;
        }
        if records_absent || absent(&self.records.join(image.as_str()))? {
            if pins_absent || absent(&self.pins.join(image.as_str()))? {
                return Ok(Status::Missing);
            }
            return Err(Error::Invalid);
        }
        let record = self.read(image)?;
        if !matches!(record.phase, Phase::Pinned) || record.paths != expected {
            return Err(Error::Invalid);
        }
        let root = self.pins.join(image.as_str());
        directory(&root)?;
        let mut actual = BTreeSet::new();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if !metadata.file_type().is_symlink()
                || metadata.uid() != rustix::process::geteuid().as_raw()
                || metadata.nlink() != 1
            {
                return Err(Error::Invalid);
            }
            let target = fs::read_link(entry.path())?;
            let target = StorePath::try_from(target.to_str().ok_or(Error::Invalid)?.to_owned())?;
            if entry.file_name() != target.basename() {
                return Err(Error::Invalid);
            }
            actual.insert(target);
        }
        if actual != expected {
            return Err(Error::Invalid);
        }
        Ok(Status::Pinned)
    }
    pub fn importing(&self, image: &Id) -> Result<Vec<StorePath>, Error> {
        let record = self.read(image)?;
        if !matches!(record.phase, Phase::Importing) {
            return Err(Error::Invalid);
        }
        if record.imports.is_empty() {
            return Err(Error::Invalid);
        }
        Ok(record.imports.into_iter().collect())
    }

    #[cfg(test)]
    pub fn begin(self, image: &Id, expected: &[StorePath]) -> Result<Pending, Error> {
        self.begin_import(image, expected, expected)
    }

    pub fn begin_import(
        self,
        image: &Id,
        expected: &[StorePath],
        imports: &[StorePath],
    ) -> Result<Pending, Error> {
        if self.inspect(image, expected)? != Status::Missing {
            return Err(Error::Invalid);
        }
        let expected = paths(expected)?;
        let import_set: BTreeSet<_> = imports.iter().cloned().collect();
        if imports.len() != import_set.len() || !import_set.is_subset(&expected) {
            return Err(Error::Invalid);
        }
        create(&self.records)?;
        create(&self.pins)?;
        let record = Record {
            version: 1,
            image: image.clone(),
            paths: expected,
            imports: import_set,
            phase: Phase::Importing,
        };
        let mut file = tempfile::NamedTempFile::new_in(&self.records)?;
        file.write_all(&serde_json::to_vec(&record)?)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(self.records.join(image.as_str()))
            .map_err(|error| error.error)?;
        File::open(&self.records)?.sync_all()?;
        Ok(Pending {
            registry: self,
            record,
        })
    }
}
impl Pending {
    pub fn finish(mut self) -> Result<(), Error> {
        let root = self.registry.pins.join(self.record.image.as_str());
        fs::DirBuilder::new().mode(0o700).create(&root)?;
        for path in &self.record.paths {
            std::os::unix::fs::symlink(path.as_str(), root.join(path.basename()))?;
        }
        File::open(&root)?.sync_all()?;
        File::open(&self.registry.pins)?.sync_all()?;
        self.record.phase = Phase::Pinned;
        let mut file = tempfile::NamedTempFile::new_in(&self.registry.records)?;
        file.write_all(&serde_json::to_vec(&self.record)?)?;
        file.as_file().sync_all()?;
        file.persist(self.registry.records.join(self.record.image.as_str()))
            .map_err(|error| error.error)?;
        File::open(&self.registry.records)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod test;
