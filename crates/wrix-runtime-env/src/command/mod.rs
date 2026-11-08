//! Image-pinned re-execution with guest-only environment overlays.
use crate::codec::{self, Payload};
use std::{
    ffi::OsString,
    fs, io,
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::Path,
    process::Command,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-runtime-env network|agent|krun-relay|krun-init [ARGUMENTS...]
    Usage,
    /// worker environment payload is unavailable; update host Wrix and the matching worker image
    Missing,
    /// worker environment payload must be a private regular file in its managed directory
    Unsafe,
    /// {0}
    Payload(#[from] codec::Error),
    /// worker environment startup failed: {0}
    Io(#[from] io::Error),
}

#[derive(Clone, Copy)]
enum Target {
    Network,
    Agent,
    KrunRelay,
    KrunInit,
}
impl Target {
    fn parse(name: &std::ffi::OsStr) -> Result<Self, Error> {
        if name == "network" {
            Ok(Self::Network)
        } else if name == "agent" {
            Ok(Self::Agent)
        } else if name == "krun-relay" {
            Ok(Self::KrunRelay)
        } else if name == "krun-init" {
            Ok(Self::KrunInit)
        } else {
            Err(Error::Usage)
        }
    }
    const fn path(self) -> &'static str {
        match self {
            Self::Network => "/network-bootstrap.sh",
            Self::Agent => "/entrypoint.sh",
            Self::KrunRelay => "/krun-relay",
            Self::KrunInit => "/krun-init.sh",
        }
    }
}

fn load(path: &Path) -> Result<Payload, Error> {
    let parent = path.parent().ok_or(Error::Unsafe)?;
    let parent = fs::symlink_metadata(parent).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            Error::Missing
        } else {
            Error::Io(error)
        }
    })?;
    if !parent.is_dir() || parent.file_type().is_symlink() || parent.mode() & 0o077 != 0 {
        return Err(Error::Unsafe);
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            Error::Missing
        } else {
            Error::Io(error)
        }
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.mode() & 0o077 != 0 {
        return Err(Error::Unsafe);
    }
    let file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.mode() & 0o077 != 0 {
        return Err(Error::Unsafe);
    }
    Ok(Payload::read(file)?)
}

fn prepare(target: Target, payload: &Payload, args: impl IntoIterator<Item = OsString>) -> Command {
    let mut command = Command::new(target.path());
    command
        .args(args)
        .envs(payload.environment())
        .env(codec::READY_VARIABLE, "1");
    command
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), Error> {
    let mut args = args.into_iter();
    let target = Target::parse(&args.next().ok_or(Error::Usage)?)?;
    let payload = load(Path::new(codec::GUEST_FILE))?;
    Err(prepare(target, &payload, args).exec().into())
}

#[cfg(test)]
mod test;
