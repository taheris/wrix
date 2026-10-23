//! Undo only byte-exact legacy runtime projections; never overwrite user edits.
use std::{fs, path::Path, process::Command};

use super::{Error, atomic_write};

pub(in crate::lifecycle) fn restore(workspace: &Path, database: &str) -> Result<(), Error> {
    let path = workspace.join(".beads/metadata.json");
    let current = fs::read(&path)?;
    let metadata: serde_json::Value = serde_json::from_slice(&current)?;
    if metadata
        .get("dolt_database")
        .and_then(serde_json::Value::as_str)
        != Some(database)
    {
        return Ok(());
    }
    let tracked = Command::new("git")
        .args(["ls-files", "--error-unmatch", "--", ".beads/metadata.json"])
        .current_dir(workspace)
        .output()?;
    if tracked.status.code() == Some(1) {
        return Ok(());
    }
    if !tracked.status.success() {
        return Err(Error::Index);
    }
    let indexed = Command::new("git")
        .args(["--no-pager", "show", ":.beads/metadata.json"])
        .current_dir(workspace)
        .output()?;
    if !indexed.status.success() {
        return Err(Error::Index);
    }
    let mut projected: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&indexed.stdout)?;
    if projected.get("backend").and_then(serde_json::Value::as_str) != Some("dolt") {
        return Ok(());
    }
    projected.insert("dolt_database".into(), database.into());
    projected.insert("dolt_mode".into(), "server".into());
    if serde_json::to_vec_pretty(&projected)? == current {
        atomic_write(&path, &indexed.stdout)?;
    }
    Ok(())
}

#[cfg(test)]
mod test;
