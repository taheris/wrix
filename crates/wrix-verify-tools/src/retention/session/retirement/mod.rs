//! A detached batch is deleted only under its independently published inode-bound record.
use super::{
    Contents, Error, Namespace, contents, exists, open_lock, owner, private_directory, regular,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, Metadata},
    io::{self, Read},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

const VERSION: &str = "wrix-ci-retirement-v1";
const LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    links: u64,
    length: u64,
}

impl Identity {
    fn capture(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
        }
    }

    fn check(&self, path: &Path, directory: bool) -> Result<(), Error> {
        let observed = fs::symlink_metadata(path)?;
        if (self.device, self.inode, self.uid, self.gid, self.mode)
            != (
                observed.dev(),
                observed.ino(),
                observed.uid(),
                observed.gid(),
                observed.mode(),
            )
            || (!directory && (self.links, self.length) != (observed.nlink(), observed.len()))
        {
            return Err(Error::Foreign(path.to_owned()));
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Kind {
    File { digest: [u8; 32] },
    Link { target: PathBuf },
    Directory,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: PathBuf,
    identity: Identity,
    kind: Kind,
}

impl Entry {
    fn capture(root: &Path, path: &Path, directory: bool) -> Result<Self, Error> {
        let metadata = fs::symlink_metadata(path)?;
        let kind = if directory {
            Kind::Directory
        } else if metadata.file_type().is_symlink() {
            Kind::Link {
                target: fs::read_link(path)?,
            }
        } else {
            Kind::File {
                digest: digest(path)?,
            }
        };
        Ok(Self {
            path: path
                .strip_prefix(root)
                .map_err(|_| Error::Foreign(path.to_owned()))?
                .to_owned(),
            identity: Identity::capture(&metadata),
            kind,
        })
    }

    fn check(&self, root: &Path) -> Result<(), Error> {
        let path = root.join(&self.path);
        self.identity
            .check(&path, matches!(self.kind, Kind::Directory))?;
        match &self.kind {
            Kind::Directory => private_directory(&path),
            Kind::File { digest: expected } => {
                regular(&path)?;
                if digest(&path)? != *expected {
                    return Err(Error::Foreign(path));
                }
                Ok(())
            }
            Kind::Link { target } if fs::read_link(&path)? == *target => Ok(()),
            Kind::Link { .. } => Err(Error::Foreign(path)),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: String,
    directory: Identity,
    entries: Vec<Entry>,
}

fn digest(path: &Path) -> Result<[u8; 32], Error> {
    let mut input = File::open(path)?;
    if input.metadata()?.len() > LIMIT {
        return Err(Error::Foreign(path.to_owned()));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 16384];
    let mut total = 0_u64;
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > LIMIT {
            return Err(Error::Foreign(path.to_owned()));
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().into())
}

fn json(error: serde_json::Error) -> Error {
    io::Error::new(io::ErrorKind::InvalidData, error).into()
}

impl Record {
    fn capture(directory: &Path, contents: Contents) -> Result<Self, Error> {
        let mut entries = Vec::new();
        for path in contents.files {
            entries.push(Entry::capture(directory, &path, false)?);
        }
        for path in contents.directories {
            entries.push(Entry::capture(directory, &path, true)?);
        }
        Ok(Self {
            version: VERSION.to_owned(),
            directory: Identity::capture(&fs::symlink_metadata(directory)?),
            entries,
        })
    }

    fn read(path: &Path) -> Result<Self, Error> {
        regular(path)?;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.mode() & 0o077 != 0 || metadata.len() > LIMIT {
            return Err(Error::Foreign(path.to_owned()));
        }
        let record: Self = serde_json::from_reader(File::open(path)?).map_err(json)?;
        let mut names = BTreeSet::new();
        if record.version != VERSION || record.directory.uid != rustix::process::getuid().as_raw() {
            return Err(Error::Foreign(path.to_owned()));
        }
        for entry in &record.entries {
            if entry.path.as_os_str().is_empty()
                || !entry
                    .path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
                || !names.insert(&entry.path)
            {
                return Err(Error::Foreign(path.to_owned()));
            }
        }
        Ok(record)
    }

    fn validate(&self, directory: &Path) -> Result<(), Error> {
        self.directory.check(directory, true)?;
        private_directory(directory)?;
        let expected = self
            .entries
            .iter()
            .map(|entry| (entry.path.as_path(), entry))
            .collect();
        Self::walk(directory, directory, &expected)
    }

    fn walk(
        root: &Path,
        directory: &Path,
        expected: &BTreeMap<&Path, &Entry>,
    ) -> Result<(), Error> {
        for child in fs::read_dir(directory)? {
            let path = child?.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| Error::Foreign(path.clone()))?;
            let entry = expected
                .get(relative)
                .ok_or_else(|| Error::Foreign(path.clone()))?;
            entry.check(root)?;
            if matches!(entry.kind, Kind::Directory) {
                Self::walk(root, &path, expected)?;
            }
        }
        Ok(())
    }

    fn remove(&self, directory: &Path, record: &Path) -> Result<(), Error> {
        if exists(directory)? {
            self.validate(directory)?;
            for entry in self
                .entries
                .iter()
                .filter(|entry| !matches!(entry.kind, Kind::Directory))
            {
                let path = directory.join(&entry.path);
                if exists(&path)? {
                    entry.check(directory)?;
                    fs::remove_file(path)?;
                }
            }
            let mut directories = self
                .entries
                .iter()
                .filter(|entry| matches!(entry.kind, Kind::Directory))
                .collect::<Vec<_>>();
            directories.sort_by_key(|entry| std::cmp::Reverse(entry.path.components().count()));
            for entry in directories {
                let path = directory.join(&entry.path);
                if exists(&path)? {
                    entry.check(directory)?;
                    fs::remove_dir(path)?;
                }
            }
            self.directory.check(directory, true)?;
            fs::remove_dir(directory)?;
        }
        // This authorization survives deletion of the batch's owner and lease files.
        regular(record)?;
        fs::remove_file(record)?;
        Ok(())
    }
}

fn suffix<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    name.strip_prefix(prefix)
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

fn stage(
    namespace: &Namespace,
    directory: &Path,
    entries: Contents,
) -> Result<(PathBuf, PathBuf, Record), Error> {
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| suffix(name, "batch-"))
        .ok_or_else(|| Error::Foreign(directory.to_owned()))?;
    let record = Record::capture(directory, entries)?;
    let record_path = namespace.path.join(format!("retirement-{name}"));
    let destination = namespace.path.join(format!("retiring-{name}"));
    let mut temporary = tempfile::Builder::new()
        .prefix(".ci-retirement-init-")
        .permissions(fs::Permissions::from_mode(0o600))
        .tempfile_in(
            namespace
                .path
                .parent()
                .ok_or_else(|| Error::Foreign(namespace.path.clone()))?,
        )?;
    serde_json::to_writer(temporary.as_file_mut(), &record).map_err(json)?;
    if temporary.as_file().metadata()?.len() > LIMIT {
        return Err(Error::Foreign(directory.to_owned()));
    }
    temporary.as_file().sync_all()?;
    for (from, to) in [
        (temporary.path(), record_path.as_path()),
        (directory, destination.as_path()),
    ] {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            from,
            rustix::fs::CWD,
            to,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)?;
    }
    Ok((destination, record_path, record))
}

pub(super) fn reap(namespace: &Namespace) -> Result<(), Error> {
    let mut batches = Vec::new();
    let mut records = BTreeMap::new();
    let mut detached = BTreeSet::new();
    for entry in fs::read_dir(&namespace.path)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| Error::Foreign(entry.path()))?;
        if matches!(name, "owner" | "lock") {
            regular(&entry.path())?;
        } else if let Some(id) = suffix(name, "retirement-") {
            records.insert(id.to_owned(), (entry.path(), Record::read(&entry.path())?));
        } else if let Some(id) = suffix(name, "retiring-") {
            detached.insert(id.to_owned());
        } else if suffix(name, "batch-").is_some() {
            owner(&entry.path())?;
            let lease = open_lock(&entry.path().join("lease"))?;
            match rustix::fs::flock(&lease, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => batches.push((entry.path(), lease, contents(&entry.path())?)),
                Err(rustix::io::Errno::WOULDBLOCK) => {}
                Err(error) => return Err(io::Error::from(error).into()),
            }
        } else {
            return Err(Error::Foreign(entry.path()));
        }
    }
    // Validate the whole namespace before retiring any of it.
    for id in &detached {
        let (_, record) = records
            .get(id)
            .ok_or_else(|| Error::Foreign(namespace.path.join(format!("retiring-{id}"))))?;
        if exists(&namespace.path.join(format!("batch-{id}")))? {
            return Err(Error::Foreign(namespace.path.join(format!("batch-{id}"))));
        }
        record.validate(&namespace.path.join(format!("retiring-{id}")))?;
    }
    for (id, (_, record)) in &records {
        let original = namespace.path.join(format!("batch-{id}"));
        if !detached.contains(id) && exists(&original)? {
            record.directory.check(&original, true)?;
        }
    }
    for (id, (path, record)) in records {
        record.remove(&namespace.path.join(format!("retiring-{id}")), &path)?;
    }
    for (directory, _lease, entries) in batches {
        let (directory, path, record) = stage(namespace, &directory, entries)?;
        record.remove(&directory, &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod test;
