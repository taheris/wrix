//! Durable component intent; an unclean epoch never authorizes another serving owner.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::Error;
use crate::control::Namespace;

const LIMIT: u64 = 4096;
const RECORD: &str = "component.json";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Intent {
    Active,
    Retired,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Epoch {
    Serving,
    Clean,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    namespace: Namespace,
    intent: Intent,
    epoch: Epoch,
}

pub(super) struct Owner {
    _lock: File,
    root: PathBuf,
    uid: u32,
    record: Record,
}

impl Owner {
    pub fn open(root: &Path, namespace: Namespace) -> Result<Self, Error> {
        wrix_core::private_directory::prepare_with_parents(root)?;
        let uid = fs::symlink_metadata(root)?.uid();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join("ownership"))?;
        check(&lock.metadata()?, uid)?;
        lock.try_lock_exclusive()?;
        let record = match read(root, uid)? {
            Some(record) => {
                if record.version != 1 || record.namespace != namespace {
                    return Err(Error::Incompatible);
                }
                if record.epoch != Epoch::Clean {
                    return Err(Error::Unclean);
                }
                record
            }
            None => Record {
                version: 1,
                namespace,
                intent: Intent::Active,
                epoch: Epoch::Clean,
            },
        };
        Ok(Self {
            _lock: lock,
            root: root.to_path_buf(),
            uid,
            record,
        })
    }

    pub const fn uid(&self) -> u32 {
        self.uid
    }

    pub const fn namespace(&self) -> &Namespace {
        &self.record.namespace
    }

    pub const fn intent(&self) -> Intent {
        self.record.intent
    }

    pub fn begin(&mut self) -> Result<(), Error> {
        self.record.epoch = Epoch::Serving;
        self.save()
    }

    pub fn intend(&mut self, intent: Intent) -> Result<(), Error> {
        self.record.intent = intent;
        self.save()
    }

    /// Call only after observing both native child exits; this is not backend work-drain evidence.
    pub fn finish(&mut self) -> Result<(), Error> {
        self.record.epoch = Epoch::Clean;
        self.save()
    }

    fn save(&self) -> Result<(), Error> {
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        temporary.write_all(&serde_json::to_vec(&self.record)?)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.root.join(RECORD))
            .map_err(|error| error.error)?;
        File::open(&self.root)?
            .sync_all()
            .map_err(Error::Committed)?;
        Ok(())
    }
}

fn read(root: &Path, uid: u32) -> Result<Option<Record>, Error> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(RECORD))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    check(&file.metadata()?, uid)?;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::Incompatible);
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn check(metadata: &fs::Metadata, uid: u32) -> Result<(), Error> {
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(Error::Permissions);
    }
    Ok(())
}

#[cfg(test)]
mod test;
