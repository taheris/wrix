//! Native stop observations do not replace original-inode storage ownership release.

use crate::lifecycle::{Error, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Role {
    Dolt,
    Celler,
    Compiler,
}

impl Role {
    const ALL: [Self; 3] = [Self::Dolt, Self::Celler, Self::Compiler];

    fn path(self, state: &Path, celler: &Path) -> PathBuf {
        match self {
            Self::Dolt => state.join("service-data/dolt/owner.lock"),
            Self::Celler => celler.join("backend.lock"),
            Self::Compiler => state.join("service-data/compiler/owner.lock"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Inode {
    device: u64,
    number: u64,
}

impl Inode {
    fn read(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            number: metadata.ino(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    role: Role,
    inode: Option<Inode>,
}

pub(super) fn extends(previous: &[Record], current: &[Record]) -> bool {
    previous.len() == current.len()
        && previous.iter().zip(current).all(|(previous, current)| {
            previous.role == current.role
                && (previous.inode.is_none() || previous.inode == current.inode)
        })
}

enum Held {
    Absent,
    Pinned(File),
}

pub(super) struct Pin {
    path: PathBuf,
    record: Record,
    held: Held,
}

impl Pin {
    pub(super) fn capture(state: &Path, celler: &Path) -> Result<Vec<Self>> {
        let uid = fs::metadata(state)?.uid();
        Role::ALL.into_iter().map(|role| {
            let path = role.path(state, celler);
            let file = match File::options().read(true).write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(&path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self {
                    path, record: Record { role, inode: None }, held: Held::Absent,
                }),
                Err(error) => return Err(error.into()),
            };
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != uid {
                return Err(Error::Operation { message: format!("{role:?} ownership is not an owned single-link original inode; storage retained") });
            }
            Ok(Self {
                path,
                record: Record { role, inode: Some(Inode::read(&metadata)) },
                held: Held::Pinned(file),
            })
        }).collect()
    }

    pub(super) fn record(&self) -> Record {
        self.record.clone()
    }

    /// Hold original kernel exclusion through durable shutdown completion; never create or replace a lock.
    pub(super) fn confirm(&self) -> Result<()> {
        match &self.held {
            Held::Absent => match fs::symlink_metadata(&self.path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
                Ok(_) => Err(Error::Operation {
                    message: format!(
                        "{:?} ownership appeared during shutdown; repeat the same stop intent without deleting native files",
                        self.record.role
                    ),
                }),
            },
            Held::Pinned(file) => {
                let metadata = file.metadata()?;
                let current = fs::symlink_metadata(&self.path)?;
                if !current.is_file()
                    || current.nlink() != 1
                    || metadata.nlink() != 1
                    || Some(Inode::read(&current)) != self.record.inode
                    || Inode::read(&metadata) != Inode::read(&current)
                {
                    return Err(Error::Operation {
                        message: format!(
                            "{:?} ownership inode changed; shutdown release remains unconfirmed",
                            self.record.role
                        ),
                    });
                }
                file.try_lock_exclusive().map_err(|error| Error::Operation {
                    message: format!(
                        "{:?} native storage ownership has not been released: {error}",
                        self.record.role
                    ),
                })
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn layout(root: &Path) -> (PathBuf, PathBuf) {
        let celler = root.join("celler");
        fs::create_dir(&celler).unwrap();
        fs::create_dir_all(root.join("service-data/dolt")).unwrap();
        let path = root.join("service-data/dolt/owner.lock");
        fs::write(&path, "retained native owner inode").unwrap();
        (celler, path)
    }

    #[test]
    fn original_native_owner_blocks_shutdown_release_until_its_kernel_lock_is_released() {
        let root = tempfile::tempdir().unwrap();
        let (celler, path) = layout(root.path());
        let owner = File::options().read(true).write(true).open(&path).unwrap();
        owner.lock_exclusive().unwrap();
        let pins = Pin::capture(root.path(), &celler).unwrap();
        assert!(pins[0].confirm().is_err());
        drop(owner);
        pins[0].confirm().unwrap();
        let competitor = File::options().read(true).write(true).open(&path).unwrap();
        assert!(competitor.try_lock_exclusive().is_err());
        drop(pins);
        competitor.try_lock_exclusive().unwrap();
    }

    #[test]
    fn replacing_an_original_ownership_inode_never_proves_native_release() {
        let root = tempfile::tempdir().unwrap();
        let (celler, path) = layout(root.path());
        let pins = Pin::capture(root.path(), &celler).unwrap();
        fs::rename(&path, root.path().join("retained-original")).unwrap();
        fs::write(&path, "unlocked replacement").unwrap();
        assert!(pins[0].confirm().is_err());
        assert_eq!(
            fs::read(root.path().join("retained-original")).unwrap(),
            b"retained native owner inode"
        );
    }

    #[test]
    fn unprovisioned_native_roles_are_not_created_by_shutdown_observation() {
        let root = tempfile::tempdir().unwrap();
        let pins = Pin::capture(root.path(), &root.path().join("absent-celler")).unwrap();
        for pin in &pins {
            pin.confirm().unwrap();
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
