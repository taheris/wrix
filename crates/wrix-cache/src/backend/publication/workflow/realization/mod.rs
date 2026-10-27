//! Selected native realization retains real out-links through synchronous publication handoff.
use super::super::{Prepared, Publisher};
use displaydoc::Display;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use thiserror::Error;

#[derive(Debug, Display, Error)]
pub enum Failure {
    /// native realization requires an explicit nonempty selected installable set
    Selection,
    /// invalid native Nix installable
    Installable,
    /// host realization I/O failed: {0}
    Io(#[from] io::Error),
    /// native Nix build failed; retained out-links at {roots:?}: {stderr}
    Build { roots: PathBuf, stderr: String },
    /// native Nix returned an invalid realization inventory; retained out-links at {0:?}
    Inventory(PathBuf),
    /// {0}
    Publication(#[from] super::super::Error),
}

/// A selected installable, never a command option or implicit default target.
pub struct Installable(String);
impl Installable {
    pub fn parse(value: String) -> Result<Self, Failure> {
        if value.is_empty() || value.starts_with('-') || value.as_bytes().contains(&0) {
            return Err(Failure::Installable);
        }
        Ok(Self(value))
    }
}

/// These native roots survive cancellation; only an explicit handoff removes them.
pub struct Rooted {
    directory: PathBuf,
    paths: Vec<PathBuf>,
}
impl Rooted {
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Call after scoped graph/publication policy selects candidates, while these out-links remain live.
    pub fn handoff(
        self,
        publisher: &Publisher,
        candidates: &[PathBuf],
    ) -> Result<Prepared, Failure> {
        let protected = publisher.prepare(candidates)?;
        fs::remove_dir_all(self.directory)?;
        Ok(protected)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Built {
    Derivation { outputs: BTreeMap<String, PathBuf> },
    Opaque(PathBuf),
}
impl Built {
    fn paths(self) -> Vec<PathBuf> {
        match self {
            Self::Derivation { outputs } => outputs.into_values().collect(),
            Self::Opaque(path) => vec![path],
        }
    }
}

/// Nix remains the executor; this never enumerates or realizes unselected targets.
pub fn realize(
    publisher: &Publisher,
    cwd: &Path,
    selected: &[Installable],
) -> Result<Rooted, Failure> {
    if selected.is_empty() {
        return Err(Failure::Selection);
    }
    let parent = publisher.state_root.join("realization-roots");
    wrix_core::private_directory::prepare_with_parents(&parent).map_err(io::Error::other)?;
    let directory = tempfile::Builder::new()
        .prefix("selected-")
        .tempdir_in(parent)?
        .keep();
    let nix = std::env::var_os("WRIX_NIX_BIN").unwrap_or_else(|| "nix".into());
    let output = Command::new(nix)
        .current_dir(cwd)
        .args(["build", "--json", "--out-link"])
        .arg(directory.join("result"))
        .args(selected.iter().map(|target| target.0.as_str()))
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(Failure::Build {
            roots: directory,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let built: Vec<Built> = serde_json::from_slice(&output.stdout)
        .map_err(|_| Failure::Inventory(directory.clone()))?;
    let paths: BTreeSet<_> = built.into_iter().flat_map(Built::paths).collect();
    if paths.is_empty()
        || paths
            .iter()
            .any(|path| path.parent() != Some(Path::new("/nix/store")))
    {
        return Err(Failure::Inventory(directory));
    }
    Ok(Rooted {
        directory,
        paths: paths.into_iter().collect(),
    })
}
