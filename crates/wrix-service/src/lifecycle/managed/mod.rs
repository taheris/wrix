use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use displaydoc::Display;
use serde_yaml_ng::{Mapping, Value};
use thiserror::Error as ThisError;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// cannot configure Wrix-managed Beads: {source}
    Io {
        #[from]
        source: io::Error,
    },
    /// invalid Beads local YAML configuration: {source}
    Yaml {
        #[from]
        source: serde_yaml_ng::Error,
    },
    /// invalid Beads metadata: {source}
    Json {
        #[from]
        source: serde_json::Error,
    },
    /// Beads configuration {key} must be a mapping
    Mapping { key: String },
}

/// Persist hook policy without changing sync settings or connecting to the database.
pub(super) fn configure(workspace: &Path) -> Result<(), Error> {
    let beads = workspace.join(".beads");
    let metadata_path = beads.join("metadata.json");
    if !beads.join("config.yaml").is_file() || !metadata_path.is_file() {
        return Ok(());
    }
    let mut metadata: serde_json::Map<String, serde_json::Value> =
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
    let ignore_path = beads.join(".gitignore");
    let mut ignore = match fs::read_to_string(&ignore_path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    if !ignore
        .lines()
        .any(|line| matches!(line.trim(), "config.local.yaml" | "/config.local.yaml"))
    {
        if !ignore.is_empty() && !ignore.ends_with('\n') {
            ignore.push('\n');
        }
        ignore.push_str("/config.local.yaml\n");
        atomic_write(&ignore_path, ignore.as_bytes())?;
    }
    atomic_write(&local_path, serde_yaml_ng::to_string(&local)?.as_bytes())?;
    if metadata
        .get("dolt_mode")
        .and_then(serde_json::Value::as_str)
        != Some("server")
    {
        metadata.insert(
            String::from("dolt_mode"),
            serde_json::Value::String(String::from("server")),
        );
        let mut bytes = serde_json::to_vec_pretty(&metadata)?;
        bytes.push(b'\n');
        atomic_write(&metadata_path, &bytes)?;
    }
    Ok(())
}

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
