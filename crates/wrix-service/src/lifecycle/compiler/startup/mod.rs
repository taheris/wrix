//! Per-start runtime markers cannot acknowledge work or release from a previous native owner.
use crate::dolt::Error;
use fs2::FileExt;
use std::{
    fs::{self, File},
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

fn lock(path: &Path) -> Result<File, Error> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(Error::UnsafePath);
    }
    file.try_lock_exclusive()?;
    Ok(file)
}

pub(super) fn prepare(cache: &Path, runtime: &Path) -> Result<File, Error> {
    let backend = runtime.join("backend");
    let compiler = runtime.join("compiler");
    crate::dolt::private_directory(&backend)?;
    crate::dolt::private_directory(&compiler)?;
    let startup = lock(&backend.join("startup.lock"))?;
    let _ownership = lock(&cache.join("owner.lock"))?;
    let paths = [
        backend.join("planned-stop"),
        compiler.join("released"),
        compiler.join("pid"),
    ];
    let mut observed = Vec::new();
    for path in &paths {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o022 != 0
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(Error::UnsafePath);
        }
        if path == &compiler.join("pid") {
            if metadata.len() > 32 {
                return Err(Error::Incomplete);
            }
            let file = File::options()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)?;
            let opened = file.metadata()?;
            if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
                return Err(Error::UnsafePath);
            }
            let mut value = String::new();
            file.take(33).read_to_string(&mut value)?;
            let value = value.strip_suffix('\n').unwrap_or(&value);
            if value.starts_with('0') || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Error::Incomplete);
            }
            let value: i32 = value.parse().map_err(|_| Error::Incomplete)?;
            let pid = rustix::process::Pid::from_raw(value).ok_or(Error::Incomplete)?;
            match rustix::process::test_kill_process(pid) {
                Err(rustix::io::Errno::SRCH) => (),
                Ok(()) => return Err(Error::Incomplete),
                Err(error) => return Err(error.into()),
            }
        }
        observed.push((path, metadata));
    }
    for (path, original) in observed {
        let current = fs::symlink_metadata(path)?;
        if !current.is_file()
            || current.dev() != original.dev()
            || current.ino() != original.ino()
            || current.nlink() != 1
            || current.uid() != original.uid()
            || current.mode() != original.mode()
            || current.ctime() != original.ctime()
            || current.ctime_nsec() != original.ctime_nsec()
        {
            return Err(Error::UnsafePath);
        }
        fs::remove_file(path)?;
    }
    File::open(backend)?.sync_all()?;
    File::open(compiler)?.sync_all()?;
    Ok(startup)
}

#[cfg(test)]
mod test;
