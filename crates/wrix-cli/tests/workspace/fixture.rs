//! Disposable host execution, including a real chroot when testing inside an image.

#![cfg(test)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[path = "isolation.rs"]
pub mod isolation;

pub struct Fixture {
    pub root: tempfile::TempDir,
    pub isolated: bool,
}

impl Fixture {
    pub fn new() -> Self {
        let (root, isolated) = isolation::root(Path::new(env!("CARGO_BIN_EXE_wrix")));
        Self { root, isolated }
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let executable = if self.isolated {
            Path::new("/bin/wrix")
        } else {
            Path::new(env!("CARGO_BIN_EXE_wrix"))
        };
        let mut command =
            isolation::command(self.root.path(), self.isolated, executable, &["workspace"]);
        command.args(args);
        command
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    pub fn state(&self) -> PathBuf {
        self.root.path().join(if cfg!(target_os = "macos") {
            "home/Library/Application Support/wrix"
        } else {
            "home/.local/state/wrix"
        })
    }
    pub fn initialize(&self) {
        let output = self.run(&["initialize"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

pub fn snapshot(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    if !path.exists() {
        return files;
    }
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            files.extend(snapshot(&entry.path()));
        } else {
            files.insert(entry.path(), fs::read(entry.path()).unwrap());
        }
    }
    files
}
