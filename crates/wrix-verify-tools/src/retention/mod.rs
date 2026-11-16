//! Bounded, explicitly owned verification caches, independent of verification verdicts.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt, symlink},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
};

const OWNER: &str = "wrix-verification-roots-v1\n";
const BATCH_SIZE: usize = 128;

fn indexed(desired: &BTreeSet<StorePath>) -> BTreeMap<String, StorePath> {
    desired
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let batch = index / BATCH_SIZE;
            let offset = index % BATCH_SIZE;
            let name = if offset == 0 {
                format!("batch-{batch}")
            } else {
                format!("batch-{batch}-{}", offset + 1)
            };
            (name, path.clone())
        })
        .collect()
}

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// verification-root I/O failed: {0}
    Io(#[from] io::Error),
    /// refusing unowned or modified verification cache: {0}
    Foreign(PathBuf),
    /// invalid top-level Nix store path: {0}
    Store(String),
    /// verification cache is leased by another process
    Busy,
    /// Nix failed while registering verification roots
    Nix,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct StorePath(PathBuf);

impl StorePath {
    pub fn parse(value: &str) -> Result<Self, Error> {
        let valid = value.strip_prefix("/nix/store/").is_some_and(|name| {
            let Some((hash, suffix)) = name.split_once('-') else {
                return false;
            };
            hash.len() == 32
                && hash
                    .bytes()
                    .all(|b| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&b))
                && !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+-._?=".contains(&b))
        });
        if !valid {
            return Err(Error::Store(value.to_owned()));
        }
        Ok(Self(PathBuf::from(value)))
    }
}

pub fn manifest(path: &Path) -> Result<BTreeSet<StorePath>, Error> {
    fs::read_to_string(path)?
        .lines()
        .map(StorePath::parse)
        .collect()
}

fn private_directory(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(Error::Foreign(path.to_path_buf()));
    }
    Ok(())
}

fn regular(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::getuid().as_raw()
    {
        return Err(Error::Foreign(path.to_path_buf()));
    }
    Ok(())
}

fn exists(path: &Path) -> Result<bool, Error> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub fn initialize(path: &Path) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Foreign(path.to_path_buf()))?;
    let temporary = tempfile::Builder::new()
        .prefix(".verification-init-")
        .tempdir_in(parent)?;
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
    fs::write(temporary.path().join("owner"), OWNER)?;
    File::create(temporary.path().join("lock"))?;
    if exists(path)? {
        return Err(Error::Foreign(path.to_path_buf()));
    }
    // A caller owns the private parent; initialization never merges an existing directory.
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    fs::rename(temporary.path().join("owner"), path.join("owner"))?;
    fs::rename(temporary.path().join("lock"), path.join("lock"))?;
    Ok(())
}

pub struct Cache {
    path: PathBuf,
    lock: File,
}

impl Cache {
    pub fn open(path: &Path) -> Result<Self, Error> {
        private_directory(path)?;
        regular(&path.join("owner"))?;
        if fs::read_to_string(path.join("owner"))? != OWNER {
            return Err(Error::Foreign(path.to_path_buf()));
        }
        let lock_path = path.join("lock");
        regular(&lock_path)?;
        let lock = File::from(
            rustix::fs::open(
                &lock_path,
                rustix::fs::OFlags::RDWR
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(io::Error::from)?,
        );
        match rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(rustix::io::Errno::WOULDBLOCK) => return Err(Error::Busy),
            Err(error) => return Err(Error::Io(error.into())),
        }
        Ok(Self {
            path: path.canonicalize()?,
            lock,
        })
    }

    fn generations(&self) -> Result<Vec<PathBuf>, Error> {
        let mut generations = Vec::new();
        for entry in fs::read_dir(&self.path)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| Error::Foreign(entry.path()))?;
            match name {
                "owner" | "lock" => regular(&entry.path())?,
                "current" | "next" => {
                    let target = fs::read_link(entry.path())?;
                    if target.components().count() != 1
                        || !target.to_string_lossy().starts_with("generation-")
                    {
                        return Err(Error::Foreign(entry.path()));
                    }
                }
                name if name.starts_with("generation-") => {
                    private_directory(&entry.path())?;
                    generations.push(entry.path());
                }
                _ => return Err(Error::Foreign(entry.path())),
            }
        }
        Ok(generations)
    }

    fn entries(directory: &Path) -> Result<Vec<(PathBuf, StorePath)>, Error> {
        let mut entries = Vec::new();
        let inventory = directory.join("manifest");
        let expected = if exists(&inventory)? {
            regular(&inventory)?;
            Some(indexed(&manifest(&inventory)?))
        } else {
            None
        };
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(Error::Foreign(entry.path()));
            };
            if name == "manifest" {
                continue;
            }
            let target = if let Some(expected) = &expected {
                expected
                    .get(name)
                    .cloned()
                    .ok_or_else(|| Error::Foreign(entry.path()))?
            } else {
                StorePath::parse(&format!("/nix/store/{name}"))?
            };
            if fs::read_link(entry.path())? != target.0 {
                return Err(Error::Foreign(entry.path()));
            }
            entries.push((entry.path(), target));
        }
        Ok(entries)
    }

    pub fn plan(
        &self,
        desired: &BTreeSet<StorePath>,
        output: &mut impl Write,
    ) -> Result<(), Error> {
        for directory in self.generations()? {
            for (root, target) in Self::entries(&directory)? {
                let action = if desired.contains(&target) {
                    "retain"
                } else {
                    "retire"
                };
                writeln!(
                    output,
                    "{action}\t{}\t{}",
                    root.display(),
                    target.0.display()
                )?;
            }
        }
        for target in desired {
            writeln!(output, "register\t{}", target.0.display())?;
        }
        Ok(())
    }

    pub fn replace(
        &self,
        desired: &BTreeSet<StorePath>,
        nix: &Path,
        output: &mut impl Write,
    ) -> Result<(), Error> {
        let old = self.generations()?;
        for directory in &old {
            Self::entries(directory)?;
        }
        let current = if exists(&self.path.join("current"))? {
            Some(self.path.join(fs::read_link(self.path.join("current"))?))
        } else {
            None
        };
        if current.as_ref().is_some_and(|path| !old.contains(path)) {
            return Err(Error::Foreign(self.path.join("current")));
        }
        for directory in &old {
            if current.as_ref() != Some(directory) {
                Self::retire(directory, output)?;
            }
        }
        let temporary = tempfile::Builder::new()
            .prefix("generation-")
            .tempdir_in(&self.path)?;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
        let generation = temporary.keep();
        let mut inventory = File::create(generation.join("manifest"))?;
        for target in desired {
            writeln!(inventory, "{}", target.0.display())?;
        }
        inventory.sync_all()?;
        let targets: Vec<_> = desired.iter().map(|path| &path.0).collect();
        for (batch, paths) in targets.chunks(BATCH_SIZE).enumerate() {
            if !Command::new(nix)
                .arg("--check-validity")
                .args(paths)
                .status()?
                .success()
            {
                return Err(Error::Nix);
            }
            if !Command::new(nix)
                .arg("--realise")
                .arg("--add-root")
                .arg(generation.join(format!("batch-{batch}")))
                .arg("--indirect")
                .args(paths)
                .stdout(std::process::Stdio::null())
                .status()?
                .success()
            {
                return Err(Error::Nix);
            }
        }
        let registered: BTreeSet<_> = Self::entries(&generation)?
            .into_iter()
            .map(|(_, target)| target)
            .collect();
        if &registered != desired {
            return Err(Error::Foreign(generation));
        }
        let next = self.path.join("next");
        if exists(&next)? {
            fs::remove_file(&next)?;
        }
        let name = generation
            .file_name()
            .ok_or_else(|| Error::Foreign(generation.clone()))?;
        symlink(name, &next)?;
        fs::rename(&next, self.path.join("current"))?;
        if let Some(directory) = current {
            Self::retire(&directory, output)?;
        }
        Ok(())
    }

    fn retire(directory: &Path, output: &mut impl Write) -> Result<(), Error> {
        for (root, target) in Self::entries(directory)? {
            if fs::read_link(&root)? != target.0 {
                return Err(Error::Foreign(root));
            }
            writeln!(output, "retire\t{}\t{}", root.display(), target.0.display())?;
            fs::remove_file(root)?;
        }
        let inventory = directory.join("manifest");
        if exists(&inventory)? {
            regular(&inventory)?;
            fs::remove_file(inventory)?;
        }
        fs::remove_dir(directory)?;
        Ok(())
    }

    pub fn execute(self, command: &mut Command) -> Result<(), Error> {
        rustix::io::fcntl_setfd(&self.lock, rustix::io::FdFlags::empty())
            .map_err(io::Error::from)?;
        Err(Error::Io(command.exec()))
    }
}

#[cfg(test)]
mod batch_test;
#[cfg(test)]
mod test;
