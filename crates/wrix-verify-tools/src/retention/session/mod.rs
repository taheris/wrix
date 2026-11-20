//! Private CI batches remain rooted only while their inherited lease is held.
pub mod frontier;
use super::{Error, StorePath, exists, private_directory, regular};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
};

const OWNER: &str = "wrix-ci-session-v1\n";
const DIRECTORY: &str = "WRIX_CI_BATCH_DIR";
const DESCRIPTOR: &str = "WRIX_CI_BATCH_LEASE_FD";
const PROCESS: &str = "WRIX_CI_SESSION_PID";

fn create_file(path: &Path, contents: &[u8]) -> Result<File, Error> {
    let mut file = OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    Ok(file)
}

fn open_lock(path: &Path) -> Result<File, Error> {
    regular(path)?;
    Ok(File::from(
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(io::Error::from)?,
    ))
}

fn owner(directory: &Path) -> Result<(), Error> {
    private_directory(directory)?;
    let path = directory.join("owner");
    regular(&path)?;
    if fs::metadata(&path)?.len() != OWNER.len() as u64 || fs::read(&path)? != OWNER.as_bytes() {
        return Err(Error::Foreign(path));
    }
    Ok(())
}

struct Namespace {
    path: PathBuf,
    _lock: File,
}

impl Namespace {
    fn open(path: &Path) -> Result<Self, Error> {
        if !path.is_absolute() {
            return Err(Error::Foreign(path.to_owned()));
        }
        if !exists(path)? {
            private_directory(
                path.parent()
                    .ok_or_else(|| Error::Foreign(path.to_owned()))?,
            )?;
            let temporary = tempfile::Builder::new()
                .prefix(".ci-session-init-")
                .permissions(fs::Permissions::from_mode(0o700))
                .tempdir_in(
                    path.parent()
                        .ok_or_else(|| Error::Foreign(path.to_owned()))?,
                )?;
            create_file(&temporary.path().join("owner"), OWNER.as_bytes())?;
            create_file(&temporary.path().join("lock"), b"")?;
            match rustix::fs::renameat_with(
                rustix::fs::CWD,
                temporary.path(),
                rustix::fs::CWD,
                path,
                rustix::fs::RenameFlags::NOREPLACE,
            ) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(io::Error::from(error).into()),
            }
        }
        owner(path)?;
        let lock = open_lock(&path.join("lock"))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
            .map_err(io::Error::from)?;
        Ok(Self {
            path: path.canonicalize()?,
            _lock: lock,
        })
    }

    fn reap(&self) -> Result<(), Error> {
        let mut retired = Vec::new();
        for entry in fs::read_dir(&self.path)? {
            let entry = entry?;
            let name = entry.file_name();
            match name.to_str() {
                Some("owner" | "lock") => regular(&entry.path())?,
                Some(name)
                    if name.strip_prefix("batch-").is_some_and(|suffix| {
                        !suffix.is_empty()
                            && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    }) =>
                {
                    owner(&entry.path())?;
                    let lease = open_lock(&entry.path().join("lease"))?;
                    match rustix::fs::flock(
                        &lease,
                        rustix::fs::FlockOperation::NonBlockingLockExclusive,
                    ) {
                        Ok(()) => retired.push((entry.path(), lease, contents(&entry.path())?)),
                        Err(rustix::io::Errno::WOULDBLOCK) => {}
                        Err(error) => return Err(io::Error::from(error).into()),
                    }
                }
                _ => return Err(Error::Foreign(entry.path())),
            }
        }
        for (directory, _lease, entries) in retired {
            for entry in entries.files {
                fs::remove_file(entry)?;
            }
            for entry in entries.directories {
                fs::remove_dir(entry)?;
            }
            fs::remove_dir(directory)?;
        }
        Ok(())
    }

    fn create(&self, program: &std::ffi::OsStr) -> Result<Batch, Error> {
        let temporary = tempfile::Builder::new()
            .prefix(".ci-batch-init-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(
                self.path
                    .parent()
                    .ok_or_else(|| Error::Foreign(self.path.clone()))?,
            )?;
        create_file(&temporary.path().join("owner"), OWNER.as_bytes())?;
        create_file(
            &temporary.path().join("program"),
            program.as_encoded_bytes(),
        )?;
        let lease = create_file(&temporary.path().join("lease"), b"")?;
        rustix::fs::flock(&lease, rustix::fs::FlockOperation::LockExclusive)
            .map_err(io::Error::from)?;
        rustix::io::fcntl_setfd(&lease, rustix::io::FdFlags::empty()).map_err(io::Error::from)?;
        let suffix = temporary
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(".ci-batch-init-"))
            .ok_or_else(|| Error::Foreign(temporary.path().to_owned()))?;
        let directory = self.path.join(format!("batch-{suffix}"));
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            temporary.path(),
            rustix::fs::CWD,
            &directory,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)?;
        Ok(Batch { directory, lease })
    }
}

struct Batch {
    directory: PathBuf,
    lease: File,
}

fn index(value: &str, prefix: &str) -> bool {
    value == prefix
        || value
            .strip_prefix(&format!("{prefix}-"))
            .is_some_and(|suffix| {
                suffix
                    .parse::<usize>()
                    .is_ok_and(|number| number >= 2 && number.to_string() == suffix)
            })
}

fn root(name: &str, target: &Path) -> Result<(), Error> {
    let value = target
        .to_str()
        .ok_or_else(|| Error::Foreign(target.to_owned()))?;
    if index(name, "derivation") {
        let output = value
            .strip_suffix(".drv")
            .ok_or_else(|| Error::Foreign(target.to_owned()))?;
        StorePath::parse(output)?;
    } else if name == "dispatcher"
        || name == "inventory-root"
        || index(name, "runner")
        || name.strip_prefix("checked-").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'.')
                })
        })
    {
        StorePath::parse(value)?;
    } else {
        return Err(Error::Foreign(target.to_owned()));
    }
    Ok(())
}

struct Contents {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

fn pending_contents(path: &Path) -> Result<Contents, Error> {
    let cache = super::Cache::open(path)?;
    let mut contents = Contents {
        files: Vec::new(),
        directories: Vec::new(),
    };
    for generation in cache.generations()? {
        let entries = fs::read_dir(&generation)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        if entries
            .iter()
            .any(|entry| entry.file_name().is_none_or(|name| name != "manifest"))
        {
            super::Cache::entries(&generation)?;
        } else {
            for entry in &entries {
                regular(entry)?;
            }
        }
        contents.files.extend(entries);
        contents.directories.push(generation);
    }
    contents.files.extend(
        fs::read_dir(path)?
            .filter_map(|entry| match entry {
                Ok(entry)
                    if entry.file_name().to_str().is_some_and(|name| {
                        matches!(name, "owner" | "lock" | "current" | "next")
                    }) =>
                {
                    Some(Ok(entry.path()))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    contents.directories.push(path.to_owned());
    Ok(contents)
}

fn contents(directory: &Path) -> Result<Contents, Error> {
    let mut paths = Contents {
        files: Vec::new(),
        directories: Vec::new(),
    };
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        match entry.file_name().to_str() {
            Some("pending") => {
                let pending = pending_contents(&path)?;
                paths.files.extend(pending.files);
                paths.directories.extend(pending.directories);
                continue;
            }
            Some(
                "owner" | "lease" | "program" | "source.json" | "identity" | "plan.json"
                | "derivations" | "graph.json" | "nodes.json" | "outputs.json",
            ) => regular(&path)?,
            Some(name) => root(
                name,
                &fs::read_link(&path).map_err(|_| Error::Foreign(path.clone()))?,
            )?,
            None => return Err(Error::Foreign(path)),
        }
        paths.files.push(path);
    }
    Ok(paths)
}

/// Reject inherited batch pointers not backed by the exact open lease and program.
pub fn check(directory: &Path, descriptor: &str, program: &std::ffi::OsStr) -> Result<(), Error> {
    owner(directory)?;
    if descriptor.is_empty() || !descriptor.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Foreign(directory.to_owned()));
    }
    regular(&directory.join("program"))?;
    if fs::metadata(directory.join("program"))?.len() != program.as_encoded_bytes().len() as u64
        || fs::read(directory.join("program"))? != program.as_encoded_bytes()
    {
        return Err(Error::Foreign(directory.to_owned()));
    }
    let lease = open_lock(&directory.join("lease"))?;
    let expected = lease.metadata()?;
    let inherited = File::from(
        rustix::fs::open(
            format!("/dev/fd/{descriptor}"),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    let observed = inherited.metadata()?;
    if (expected.dev(), expected.ino()) != (observed.dev(), observed.ino()) {
        return Err(Error::Foreign(directory.to_owned()));
    }
    match rustix::fs::flock(&lease, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(()),
        Ok(()) => Err(Error::Foreign(directory.to_owned())),
        Err(error) => Err(io::Error::from(error).into()),
    }
}

/// Retire only batches whose ordinary descendants no longer hold their lease.
pub fn clean(path: &Path) -> Result<(), Error> {
    owner(path)?;
    Namespace::open(path)?.reap()
}

/// Replace this process while keeping the batch leased through ordinary descendants.
pub fn execute(path: &Path, command: &mut Command) -> Result<(), Error> {
    let namespace = Namespace::open(path)?;
    namespace.reap()?;
    let batch = namespace.create(command.get_program())?;
    drop(namespace);
    let error = command
        .env(DIRECTORY, &batch.directory)
        .env(DESCRIPTOR, batch.lease.as_raw_fd().to_string())
        .env(PROCESS, std::process::id().to_string())
        .exec();
    drop(batch);
    clean(path)?;
    Err(error.into())
}

#[cfg(test)]
mod test;
