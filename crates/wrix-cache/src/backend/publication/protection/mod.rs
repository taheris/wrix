//! Real native host GC roots retained until publication completes or is reconciled.
use displaydoc::Display;
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use thiserror::Error;

#[derive(Debug, Display, Error)]
pub enum Failure {
    /// host publication protection I/O failed: {0}
    Io(#[from] io::Error),
    /// publication protection accepts only canonical non-derivation Nix output paths
    Path,
    /// native Nix failed to protect a realized publication output: {0}
    Native(String),
}

/// No destructor releases roots: cancellation and caller loss are not completion.
pub struct Protection {
    directory: PathBuf,
}
impl Protection {
    /// The caller must retain originating temporary roots until this returns.
    pub fn acquire(state_root: &Path, paths: &[PathBuf]) -> Result<Self, Failure> {
        for path in paths {
            if path.parent() != Some(Path::new("/nix/store"))
                || path
                    .file_name()
                    .is_none_or(|name| name.as_encoded_bytes().ends_with(b".drv"))
            {
                return Err(Failure::Path);
            }
        }
        let parent = state_root.join("publication-roots");
        wrix_core::private_directory::prepare_with_parents(&parent).map_err(io::Error::other)?;
        let directory = tempfile::Builder::new()
            .prefix("inflight-")
            .tempdir_in(parent)?
            .keep();
        let protection = Self { directory };
        for (index, path) in paths.iter().enumerate() {
            let output = Command::new("nix-store")
                .args(["--option", "substitute", "false", "--add-root"])
                .arg(protection.directory.join(index.to_string()))
                .args(["--indirect", "--realise"])
                .arg(path)
                .stdin(Stdio::null())
                .output()?;
            if !output.status.success() {
                return Err(Failure::Native(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
        }
        Ok(protection)
    }

    /// Only authoritative completion permits removing these native roots.
    pub fn release(self) -> Result<(), Failure> {
        fs::remove_dir_all(&self.directory)?;
        Ok(())
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}
