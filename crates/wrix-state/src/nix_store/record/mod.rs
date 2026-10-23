//! A host-recorded membership, not a filesystem projection or live owner.

use super::Error;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
    },
    path::{Component, PathBuf},
};
use wrix_core::{nix_store::pool::Repository, path::WorkspaceHash};

pub(super) const OWNER: &str = "installed-store-approval";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Common {
    path: Vec<u8>,
    device: u64,
    inode: u64,
}

impl Common {
    pub(super) fn observe(context: &crate::context::Context) -> Result<Self, Error> {
        let checkout = context
            .primary_checkout()
            .ok_or(Error::RepositoryRequired)?;
        checkout.recheck()?;
        let path = checkout.common();
        let metadata = path.metadata()?;
        Ok(Self {
            path: path.as_os_str().as_bytes().to_vec(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(super) fn recheck(&self) -> Result<(), Error> {
        self.check()?;
        let path = PathBuf::from(OsString::from_vec(self.path.clone()));
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_dir()
            || path.canonicalize()? != path
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
        {
            return Err(Error::RepositoryChanged);
        }
        Ok(())
    }

    const fn same_object(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }

    fn check(&self) -> Result<(), Error> {
        let path = PathBuf::from(OsString::from_vec(self.path.clone()));
        if !path.is_absolute()
            || self.path.contains(&0)
            || path
                .components()
                .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
            || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
        {
            return Err(Error::Unsupported);
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    version: u8,
    pub repository: Repository,
    pub common: Common,
    pub enabled: bool,
}

impl Record {
    pub(super) const fn new(repository: Repository, common: Common, enabled: bool) -> Self {
        Self {
            version: 1,
            repository,
            common,
            enabled,
        }
    }

    pub(super) fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let record: Self = serde_json::from_slice(bytes)?;
        if record.version != 1 {
            return Err(Error::Unsupported);
        }
        record.common.check()?;
        Ok(record)
    }
}

pub(super) struct Catalog(pub Vec<(WorkspaceHash, Record)>);

impl Catalog {
    pub(super) fn load(store: &crate::store::Store) -> Result<Self, Error> {
        let mut statement = store.connection.prepare("SELECT native_key, length(value), value FROM namespace_metadata WHERE owner = ?1 ORDER BY native_key")?;
        let mut rows = statement.query([OWNER])?;
        let mut records: Vec<(WorkspaceHash, Record)> = Vec::new();
        while let Some(row) = rows.next()? {
            let length: i64 = row.get(1)?;
            if !(1..=65536).contains(&length) {
                return Err(Error::Unsupported);
            }
            let key: String = row.get(0)?;
            let key = WorkspaceHash::parse(&key).map_err(|_| Error::Unsupported)?;
            let bytes: Vec<u8> = row.get(2)?;
            let record = Record::parse(&bytes)?;
            for (_, prior) in &records {
                if prior.common.same_object(&record.common) && prior.common != record.common {
                    return Err(Error::Inconsistent);
                }
                if (prior.repository == record.repository
                    && (prior.common != record.common || prior.enabled != record.enabled))
                    || (prior.common == record.common && prior.repository != record.repository)
                {
                    return Err(Error::Inconsistent);
                }
            }
            records.push((key, record));
        }
        Ok(Self(records))
    }

    pub(super) fn selected(
        &self,
        context: &crate::context::Context,
        common: &Common,
    ) -> Result<Option<&Record>, Error> {
        if self
            .0
            .iter()
            .any(|(_, record)| record.common.same_object(common) && &record.common != common)
        {
            return Err(Error::RepositoryChanged);
        }
        let record = self
            .0
            .iter()
            .find(|(key, _)| key == context.key())
            .map(|(_, record)| record);
        if record.is_some_and(|record| &record.common != common) {
            return Err(Error::RepositoryChanged);
        }
        Ok(record)
    }
}
