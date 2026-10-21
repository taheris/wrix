//! Pin a checked mount source without following a replaceable path component.

use super::Roots;
use rustix::fs::{Mode, OFlags, open, openat};
use std::{
    fs::File,
    path::{Component, Path},
};

#[derive(Clone, Copy)]
pub enum MountAccess {
    Read,
    ReadWrite,
}

impl Roots {
    pub fn open_mount(&self, source: &Path, access: MountAccess) -> std::io::Result<File> {
        let source = source.canonicalize()?;
        self.protect_mount(&source)?;
        let mut directory = open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let mut components = source.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                continue;
            };
            let flags = if components.peek().is_some() {
                OFlags::RDONLY | OFlags::DIRECTORY
            } else {
                (match access {
                    MountAccess::Read => OFlags::RDONLY,
                    MountAccess::ReadWrite => OFlags::RDWR,
                }) | OFlags::NONBLOCK
            };
            directory = openat(
                &directory,
                name,
                flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
        }
        let source = File::from(directory);
        let metadata = source.metadata()?;
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsupported mount source type",
            ));
        }
        Ok(source)
    }
}
