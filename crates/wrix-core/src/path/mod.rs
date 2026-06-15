use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    canonical_path: PathBuf,
}

impl Workspace {
    pub fn from_current_dir() -> io::Result<Self> {
        let current_dir = std::env::current_dir()?;
        Self::from_path(current_dir)
    }

    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let canonical_path = path.as_ref().canonicalize()?;
        Ok(Self { canonical_path })
    }

    pub fn canonical_path(&self) -> &Path {
        self.canonical_path.as_path()
    }

    pub fn repository_name(&self) -> Option<&OsStr> {
        self.canonical_path.file_name()
    }
}
