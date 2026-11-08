//! Docker archive streaming from cached OCI blobs without nested producers.
use super::{Digest, Error, archive, timestamp};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct Configuration {
    repo_tag: String,
    mtime: String,
}

#[derive(Deserialize)]
struct Source {
    wrix_oci_layout: PathBuf,
    wrix_parent_layout: Option<PathBuf>,
}

pub fn configured(conf: &Path, tag: Option<&str>, output: &mut impl Write) -> Result<(), Error> {
    let source: Source = serde_json::from_slice(&fs::read(conf)?)?;
    stream(
        &source.wrix_oci_layout,
        conf,
        source.wrix_parent_layout.as_deref(),
        tag,
        output,
    )
}

struct Counting<W> {
    output: W,
    size: u64,
}
impl<W: Write> Write for Counting<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.output.write(bytes)?;
        self.size = self
            .size
            .checked_add(u64::try_from(written).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("archive size overflow"))?;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

pub fn stream(
    layout: &Path,
    conf: &Path,
    parent: Option<&Path>,
    tag: Option<&str>,
    output: &mut impl Write,
) -> Result<(), Error> {
    let conf: Configuration = serde_json::from_slice(&fs::read(conf)?)?;
    let mtime = timestamp(&conf.mtime)?.timestamp_micros() / 1_000_000;
    let manifest: Value = serde_json::from_slice(&fs::read(layout.join("wrix/manifest.json"))?)?;
    let config_digest = Digest::prefixed(
        manifest
            .pointer("/config/digest")
            .and_then(Value::as_str)
            .ok_or(Error::Metadata)?,
    )?;
    let mut config: Value = serde_json::from_slice(&fs::read(
        layout.join("blobs/sha256").join(config_digest.0),
    )?)?;
    let inherited = if let Some(parent) = parent {
        let manifest: Value =
            serde_json::from_slice(&fs::read(parent.join("wrix/manifest.json"))?)?;
        manifest["layers"].as_array().ok_or(Error::Metadata)?.len()
    } else {
        0
    };
    let history = config["history"].as_array_mut().ok_or(Error::Metadata)?;
    if inherited > history.len() {
        return Err(Error::Metadata);
    }
    for entry in &mut history[..inherited] {
        entry["comment"] = json!("store paths: None");
    }
    let bytes = serde_json::to_vec_pretty(&config)?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let config_name = format!("{digest}.json");
    let layers = manifest["layers"].as_array().ok_or(Error::Metadata)?;
    let mut names = Vec::new();
    let mut output = Counting { output, size: 0 };
    for layer in layers {
        let digest = Digest::prefixed(layer["digest"].as_str().ok_or(Error::Metadata)?)?;
        let size = layer["size"].as_u64().ok_or(Error::Metadata)?;
        let source = layout.join("blobs/sha256").join(&digest.0);
        let properties = fs::metadata(&source)?;
        if !properties.is_file() || properties.len() != size {
            return Err(Error::Metadata);
        }
        let name = format!("{}/layer.tar", digest.0);
        archive::append_blob(&mut output, &name, &source, size, mtime)?;
        names.push(name);
    }
    archive::append_bytes(&mut output, &config_name, &bytes, mtime)?;
    let manifest = json!([{"Config": config_name, "RepoTags": [tag.unwrap_or(&conf.repo_tag)], "Layers": names}]);
    archive::append_bytes(
        &mut output,
        "manifest.json",
        &serde_json::to_vec_pretty(&manifest)?,
        mtime,
    )?;
    let size = output.size;
    archive::finish_export(&mut output, size)?;
    Ok(())
}

#[cfg(test)]
mod test;
