use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

use displaydoc::Display;
use serde_yaml_ng::{Mapping, Value};
use thiserror::Error as ThisError;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// cannot configure Wrix-managed Beads: {source}
    Io { source: io::Error },
    /// invalid Beads local YAML configuration: {source}
    Yaml { source: serde_yaml_ng::Error },
    /// invalid Beads metadata: {source}
    Json { source: serde_json::Error },
    /// cannot inspect tracked Beads metadata in the Git index; working file left untouched
    Index,
    /// cannot resolve local Git ignore policy for managed Beads configuration
    Ignore,
    /// Beads configuration {key} must be a mapping
    Mapping { key: String },
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

impl From<serde_yaml_ng::Error> for Error {
    fn from(source: serde_yaml_ng::Error) -> Self {
        Self::Yaml { source }
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Self::Json { source }
    }
}

/// Persist hook policy without changing sync settings or connecting to the database.
pub(super) fn configure(workspace: &Path) -> Result<(), Error> {
    let beads = workspace.join(".beads");
    let metadata_path = beads.join("metadata.json");
    if !beads.join("config.yaml").is_file() || !metadata_path.is_file() {
        return Ok(());
    }
    let metadata: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&fs::read(&metadata_path)?)?;
    if metadata.get("backend").and_then(serde_json::Value::as_str) != Some("dolt") {
        return Ok(());
    }
    let local_path = beads.join("config.local.yaml");
    let mut local = match fs::read(&local_path) {
        Ok(bytes) => serde_yaml_ng::from_slice::<Option<Mapping>>(&bytes)?.unwrap_or_default(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Mapping::new(),
        Err(error) => return Err(error.into()),
    };
    set(&mut local, "dolt", "auto-start", Value::Bool(false))?;
    set(
        &mut local,
        "dolt",
        "mode",
        Value::String(String::from("server")),
    )?;
    set(&mut local, "import", "auto", Value::Bool(false))?;
    set(&mut local, "export", "auto", Value::Bool(false))?;
    ignore_local_config(workspace)?;
    atomic_write(&local_path, serde_yaml_ng::to_string(&local)?.as_bytes())?;
    Ok(())
}

fn ignore_local_config(workspace: &Path) -> Result<(), Error> {
    let ignored = Command::new("git")
        .args([
            "check-ignore",
            "--quiet",
            "--no-index",
            "--",
            ".beads/config.local.yaml",
        ])
        .current_dir(workspace)
        .output()?;
    if ignored.status.success() {
        return Ok(());
    }
    if ignored.status.code() != Some(1) {
        return Err(Error::Ignore);
    }
    let location = Command::new("git")
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/exclude",
        ])
        .current_dir(workspace)
        .output()?;
    if !location.status.success() {
        return Err(Error::Ignore);
    }
    let path = PathBuf::from(
        String::from_utf8(location.stdout)
            .map_err(|_| Error::Ignore)?
            .trim(),
    );
    if !path.is_absolute() {
        return Err(Error::Ignore);
    }
    let mut contents = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str("/.beads/config.local.yaml\n");
    fs::create_dir_all(path.parent().ok_or(Error::Ignore)?)?;
    atomic_write(&path, contents.as_bytes())?;
    let checked = Command::new("git")
        .args([
            "check-ignore",
            "--quiet",
            "--no-index",
            "--",
            ".beads/config.local.yaml",
        ])
        .current_dir(workspace)
        .output()?;
    if !checked.status.success() {
        return Err(Error::Ignore);
    }
    Ok(())
}

pub(super) mod metadata;

#[cfg(test)]
mod test;

fn set(mapping: &mut Mapping, section: &str, key: &str, value: Value) -> Result<(), Error> {
    let section_key = Value::String(section.to_owned());
    if let Some(section_value) = mapping.get_mut(&section_key) {
        let fields = section_value
            .as_mapping_mut()
            .ok_or_else(|| Error::Mapping {
                key: section.to_owned(),
            })?;
        fields.remove(Value::String(key.to_owned()));
        if fields.is_empty() {
            mapping.remove(&section_key);
        }
    }
    // Viper gives dotted keys precedence over nested keys even across merged files.
    mapping.insert(Value::String(format!("{section}.{key}")), value);
    Ok(())
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if path.is_file() && fs::read(path)? == bytes {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("configuration path has no parent"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}
