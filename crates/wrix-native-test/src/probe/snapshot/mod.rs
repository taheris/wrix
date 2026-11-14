use rustix::fs::{Mode, OFlags};
use std::{
    fs::{self, File},
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::Path,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// operator snapshot I/O failed: {0}
    Io(#[from] io::Error),
    /// operator snapshot descriptor access failed: {0}
    Descriptor(#[from] rustix::io::Errno),
    /// operator snapshot must be an owned canonical private directory
    Directory,
    /// operator snapshot file must be owned, private, single-linked and bounded
    File,
}

pub struct Directory(File);
impl Directory {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let before = fs::symlink_metadata(path)?;
        if !before.is_dir()
            || before.uid() != rustix::process::getuid().as_raw()
            || before.mode() & 0o077 != 0
            || fs::canonicalize(path)? != path
        {
            return Err(Error::Directory);
        }
        let fd = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let file = File::from(fd);
        let after = file.metadata()?;
        if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
            return Err(Error::Directory);
        }
        Ok(Self(file))
    }
    pub fn read(&self, name: &str) -> Result<Vec<u8>, Error> {
        if name.is_empty() || name == "." || name == ".." || name.as_bytes().contains(&b'/') {
            return Err(Error::File);
        }
        let fd = rustix::fs::openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let file = File::from(fd);
        let before = file.metadata()?;
        let bound = 1024 * 1024;
        if !before.is_file()
            || before.uid() != rustix::process::getuid().as_raw()
            || before.mode() & 0o777 != 0o600
            || before.nlink() != 1
            || before.len() > bound
        {
            return Err(Error::File);
        }
        let mut bytes = Vec::new();
        let mut file = file.take(bound + 1);
        file.read_to_end(&mut bytes)?;
        let after = file.get_ref().metadata()?;
        if bytes.len() as u64 > bound
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
        {
            return Err(Error::File);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod test;
