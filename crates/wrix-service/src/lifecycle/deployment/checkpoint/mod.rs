//! A quiescent native checkpoint is recovery evidence, never permission to restart an old binary after migration.

use super::{document, publish};
use crate::lifecycle::{Error, Result, binding, owner};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io,
    path::Path,
};
use wrix_sandbox::image::Digest;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u16,
    target: binding::Candidate,
    digest: Digest,
    source: Option<Digest>,
}

/// Caller retains original native backend ownership exclusion through copy and durability publication.
pub(super) fn create(
    state: &Path,
    attempt: &super::update::Attempt,
    target: &binding::Candidate,
    digest: &Digest,
    source: Option<&Digest>,
) -> Result<()> {
    let key = attempt.as_str();
    let root = state.join("host-lifecycle/checkpoints");
    wrix_core::private_directory::prepare_with_parents(&root)?;
    let complete = root.join(key);
    if complete.try_exists()? {
        return verify(state, attempt, target, digest, source);
    }
    let building = tempfile::tempdir_in(&root)?;
    let databases = state.join("service-data/dolt/databases");
    if databases.try_exists()? {
        copy(&databases, &building.path().join("dolt"))?;
    } else if state
        .join("service-data/dolt/admin.password")
        .try_exists()?
    {
        return Err(owner::Error::Invalid.into());
    } else {
        fs::create_dir(building.path().join("dolt"))?;
    }
    let layout = wrix_cache::backend::layout::Layout::current()?;
    copy(layout.metadata(), &building.path().join("celler"))?;
    publish(
        building.path(),
        "checkpoint.json",
        &Receipt {
            version: 1,
            target: target.clone(),
            digest: digest.clone(),
            source: source.cloned(),
        },
    )?;
    fs::rename(building.path(), &complete)?;
    File::open(&root)?.sync_all()?;
    Ok(())
}

pub(super) fn verify(
    state: &Path,
    attempt: &super::update::Attempt,
    target: &binding::Candidate,
    digest: &Digest,
    source: Option<&Digest>,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let root = state
        .join("host-lifecycle/checkpoints")
        .join(attempt.as_str());
    let metadata = fs::symlink_metadata(&root)?;
    if !metadata.is_dir()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != fs::metadata(state)?.uid()
        || root.canonicalize()? != root
    {
        return Err(owner::Error::Permissions.into());
    }
    let bytes = document(&root, "checkpoint.json")?.ok_or(owner::Error::Invalid)?;
    let receipt: Receipt = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if receipt.version != 1
        || receipt.target != *target
        || receipt.digest != *digest
        || receipt.source.as_ref() != source
    {
        return Err(owner::Error::Conflict.into());
    }
    Ok(())
}

fn copy(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Err(Error::Operation { message: "native checkpoint refuses symbolic storage links rather than following service-controlled host paths".into() });
    } else if metadata.is_dir() {
        fs::create_dir(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if entry.file_name() == "backend.lock" || entry.file_name() == "owner.lock" {
                continue;
            }
            copy(&entry.path(), &target.join(entry.file_name()))?;
        }
        File::open(target)?.sync_all()?;
    } else if metadata.is_file() {
        fs::copy(source, target)?;
        fs::set_permissions(target, metadata.permissions())?;
        File::open(target)?.sync_all()?;
    } else {
        return Err(Error::Operation {
            message: "native checkpoint encountered a non-storage entry; no candidate was started"
                .into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn checkpoint_copy_does_not_follow_service_symlinks_or_replace_native_ownership() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("metadata.db"), "native data").unwrap();
        fs::write(source.join("backend.lock"), "original inode").unwrap();
        let private = root.path().join("host-private");
        fs::write(&private, "not imported").unwrap();
        std::os::unix::fs::symlink(&private, source.join("alias")).unwrap();
        let output = root.path().join("checkpoint");
        assert!(copy(&source, &output).is_err());
        assert!(!output.join("alias").exists());
        assert!(!output.join("backend.lock").exists());
        assert_eq!(fs::read(private).unwrap(), b"not imported");
        assert_eq!(
            fs::read(source.join("backend.lock")).unwrap(),
            b"original inode"
        );
    }
}
