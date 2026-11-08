//! Deterministic OCI metadata composition with immutable cached layer references.
mod archive;
pub mod docker;

use chrono::{DateTime, FixedOffset, NaiveDateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    ffi::OsString,
    fs, io,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    time::SystemTime,
};

const LAYER: &str = "application/vnd.oci.image.layer.v1.tar";
const CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const INDEX: &str = "application/vnd.oci.image.index.v1+json";

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-image-tools oci-layout CONF OUTPUT [--from-layout PATH] [--store-layers PATH] [--store-layers-only]
    Usage,
    /// OCI filesystem operation failed: {0}
    Io(#[from] io::Error),
    /// OCI metadata is invalid: {0}
    Json(#[from] serde_json::Error),
    /// OCI assembly requires an OCI parent layout, not a whole-image archive
    ArchiveParent,
    /// OCI SHA-256 digest is invalid
    Digest,
    /// OCI timestamp is invalid
    Timestamp,
    /// OCI numeric ownership is invalid
    Ownership,
    /// OCI metadata has an invalid shape
    Metadata,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(try_from = "String")]
struct Digest(String);
impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
impl TryFrom<String> for Digest {
    type Error = Error;
    fn try_from(text: String) -> Result<Self, Error> {
        if text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(text))
        } else {
            Err(Error::Digest)
        }
    }
}
impl Digest {
    fn prefixed(text: &str) -> Result<Self, Error> {
        text.strip_prefix("sha256:")
            .ok_or(Error::Digest)?
            .to_owned()
            .try_into()
    }
    fn reference(&self) -> String {
        format!("sha256:{}", self.0)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Ownership {
    Text(String),
    Number(u64),
}
impl Ownership {
    fn number(&self) -> Result<u64, Error> {
        match self {
            Self::Text(text) => text.parse().map_err(|_| Error::Ownership),
            Self::Number(value) => Ok(*value),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum Provenance {
    Paths(Vec<PathBuf>),
    Comment(String),
}
impl Provenance {
    fn comment(&self) -> Result<String, Error> {
        match self {
            Self::Comment(comment) => Ok(comment.clone()),
            Self::Paths(paths) => {
                let values = paths
                    .iter()
                    .map(|path| path.to_str().ok_or(Error::Metadata).map(python_repr))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("store paths: [{}]", values.join(", ")))
            }
        }
    }
}

fn python_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut output = String::from(quote);
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character == quote => {
                output.push('\\');
                output.push(character);
            }
            character if character.is_control() => {
                let number = u32::from(character);
                let escaped = if number <= 0xff {
                    format!("\\x{number:02x}")
                } else if number <= 0xffff {
                    format!("\\u{number:04x}")
                } else {
                    format!("\\U{number:08x}")
                };
                output.push_str(&escaped);
            }
            character => output.push(character),
        }
    }
    output.push(quote);
    output
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Layer {
    digest: Digest,
    size: u64,
    paths: Provenance,
}
impl Layer {
    fn descriptor(&self) -> Value {
        json!({"mediaType": LAYER, "digest": self.digest.reference(), "size": self.size})
    }
}

#[derive(Deserialize)]
struct Configuration {
    from_image: Option<Value>,
    created: String,
    mtime: String,
    uid: Ownership,
    gid: Ownership,
    uname: String,
    gname: String,
    store_layers: Vec<Vec<PathBuf>>,
    customisation_layer: PathBuf,
    architecture: String,
    os: String,
    config: Map<String, Value>,
    repo_tag: String,
}

pub struct Options {
    conf: PathBuf,
    output: PathBuf,
    parent: Option<PathBuf>,
    store: Option<PathBuf>,
    mode: Mode,
}
#[derive(Clone, Copy)]
enum Mode {
    Layout,
    StoreLayers,
}
impl Options {
    pub fn parse(args: &[OsString]) -> Result<Self, Error> {
        let [conf, output, options @ ..] = args else {
            return Err(Error::Usage);
        };
        let mut parent = None;
        let mut store = None;
        let mut mode = Mode::Layout;
        let mut options = options.iter();
        while let Some(option) = options.next() {
            if option == "--from-layout" && parent.is_none() {
                parent = Some(PathBuf::from(options.next().ok_or(Error::Usage)?));
            } else if option == "--store-layers" && store.is_none() {
                store = Some(PathBuf::from(options.next().ok_or(Error::Usage)?));
            } else if option == "--store-layers-only" && matches!(mode, Mode::Layout) {
                mode = Mode::StoreLayers;
            } else {
                return Err(Error::Usage);
            }
        }
        Ok(Self {
            conf: conf.into(),
            output: output.into(),
            parent,
            store,
            mode,
        })
    }
}

fn timestamp(text: &str) -> Result<DateTime<FixedOffset>, Error> {
    if text == "now" {
        return Ok(DateTime::<Utc>::from(SystemTime::now()).fixed_offset());
    }
    DateTime::parse_from_rfc3339(text)
        .or_else(|_| {
            NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
                .map(|date| date.and_utc().fixed_offset())
        })
        .map_err(|_| Error::Timestamp)
}

fn created(text: &str) -> Result<String, Error> {
    let time = timestamp(text)?;
    let base = time.format("%Y-%m-%dT%H:%M:%S");
    let fraction = if time.nanosecond() / 1000 == 0 {
        String::new()
    } else {
        format!(".{:06}", time.nanosecond() / 1000)
    };
    let offset =
        if text != "now" && NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").is_ok() {
            String::new()
        } else {
            time.format("%:z").to_string()
        };
    Ok(format!("{base}{fraction}{offset}"))
}

fn link_blob(source: &Path, blobs: &Path, digest: &Digest) -> Result<(), Error> {
    let target = blobs.join(&digest.0);
    match fs::symlink_metadata(&target) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            symlink(std::path::absolute(source)?, target)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn write_blob(bytes: &[u8], blobs: &Path) -> Result<Value, Error> {
    let digest = Digest(format!("{:x}", Sha256::digest(bytes)));
    let target = blobs.join(&digest.0);
    if !target.try_exists()? {
        fs::write(target, bytes)?;
    }
    Ok(json!({"digest": digest.reference(), "size": bytes.len()}))
}

fn overlay(
    base: &Map<String, Value>,
    mut final_config: Map<String, Value>,
) -> Result<Map<String, Value>, Error> {
    let mut env: Vec<String> = Vec::new();
    for config in [base, &final_config] {
        if let Some(values) = config.get("Env") {
            for value in values.as_array().ok_or(Error::Metadata)? {
                let value = value.as_str().ok_or(Error::Metadata)?;
                let name = value.split('=').next().ok_or(Error::Metadata)?;
                if let Some(previous) = env
                    .iter_mut()
                    .find(|entry| entry.split('=').next() == Some(name))
                {
                    value.clone_into(previous);
                } else {
                    env.push(value.to_owned());
                }
            }
        }
    }
    if !env.is_empty() {
        final_config.insert("Env".to_owned(), json!(env));
    }
    Ok(final_config)
}

fn parent_layers(parent: &Path, blobs: &Path) -> Result<(Vec<Layer>, Map<String, Value>), Error> {
    let manifest: Value = serde_json::from_slice(&fs::read(parent.join("wrix/manifest.json"))?)?;
    let digest = Digest::prefixed(
        manifest
            .pointer("/config/digest")
            .and_then(Value::as_str)
            .ok_or(Error::Metadata)?,
    )?;
    let config: Value =
        serde_json::from_slice(&fs::read(parent.join("blobs/sha256").join(digest.0))?)?;
    let descriptors = manifest["layers"].as_array().ok_or(Error::Metadata)?;
    let history = config["history"].as_array().ok_or(Error::Metadata)?;
    if descriptors.len() != history.len() {
        return Err(Error::Metadata);
    }
    let layers = descriptors
        .iter()
        .zip(history)
        .map(|(descriptor, history)| {
            let digest = Digest::prefixed(descriptor["digest"].as_str().ok_or(Error::Metadata)?)?;
            link_blob(&parent.join("blobs/sha256").join(&digest.0), blobs, &digest)?;
            Ok(Layer {
                digest,
                size: descriptor["size"].as_u64().ok_or(Error::Metadata)?,
                paths: Provenance::Comment(
                    history["comment"]
                        .as_str()
                        .ok_or(Error::Metadata)?
                        .to_owned(),
                ),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let base = match config.get("config") {
        Some(value) => value.as_object().ok_or(Error::Metadata)?.clone(),
        None => Map::new(),
    };
    Ok((layers, base))
}

pub fn build(options: &Options) -> Result<(), Error> {
    let conf: Configuration = serde_json::from_slice(&fs::read(&options.conf)?)?;
    if conf.from_image.is_some() {
        return Err(Error::ArchiveParent);
    }
    let created = created(&conf.created)?;
    let mtime = timestamp(&conf.mtime)?.timestamp_micros() / 1_000_000;
    let ownership = archive::Ownership {
        uid: conf.uid.number()?,
        gid: conf.gid.number()?,
        uname: &conf.uname,
        gname: &conf.gname,
    };
    let parent = options
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let work = tempfile::Builder::new()
        .prefix(".wrix-oci-")
        .tempdir_in(parent)?;
    let blobs = work.path().join("blobs/sha256");
    let wrix = work.path().join("wrix");
    fs::create_dir_all(&blobs)?;
    fs::create_dir(&wrix)?;
    let store_layers: Vec<Layer> = if let Some(store) = &options.store {
        let layers: Vec<Layer> = serde_json::from_slice(&fs::read(store.join("layers.json"))?)?;
        for layer in &layers {
            link_blob(
                &store.join("blobs/sha256").join(&layer.digest.0),
                &blobs,
                &layer.digest,
            )?;
        }
        layers
    } else {
        conf.store_layers
            .iter()
            .map(|paths| archive::write(paths, &blobs, mtime, &ownership))
            .collect::<Result<_, _>>()?
    };
    if matches!(options.mode, Mode::StoreLayers) {
        fs::write(
            work.path().join("layers.json"),
            serde_json::to_vec(&store_layers)?,
        )?;
        fs::rename(work.path(), &options.output)?;
        return Ok(());
    }
    let (mut layers, base) = if let Some(parent) = &options.parent {
        parent_layers(parent, &blobs)?
    } else {
        (Vec::new(), Map::new())
    };
    layers.extend(store_layers);
    let custom = &conf.customisation_layer;
    let custom_digest: Digest = fs::read_to_string(custom.join("checksum"))?
        .trim()
        .to_owned()
        .try_into()?;
    let size = fs::metadata(custom.join("layer.tar"))?.len();
    link_blob(&custom.join("layer.tar"), &blobs, &custom_digest)?;
    layers.push(Layer {
        digest: custom_digest,
        size,
        paths: Provenance::Paths(vec![custom.clone()]),
    });
    let history = layers
        .iter()
        .map(|layer| Ok(json!({"created": created, "comment": layer.paths.comment()?})))
        .collect::<Result<Vec<_>, Error>>()?;
    let config = json!({"created": created, "architecture": conf.architecture, "os": conf.os, "config": overlay(&base, conf.config)?, "rootfs": {"type": "layers", "diff_ids": layers.iter().map(|layer| layer.digest.reference()).collect::<Vec<_>>()}, "history": history});
    let mut config_descriptor = write_blob(&serde_json::to_vec_pretty(&config)?, &blobs)?;
    config_descriptor["mediaType"] = json!(CONFIG);
    let descriptors: Vec<_> = layers.iter().map(Layer::descriptor).collect();
    let manifest = json!({"schemaVersion": 2, "mediaType": MANIFEST, "config": config_descriptor, "layers": descriptors});
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    let mut manifest_descriptor = write_blob(&manifest_bytes, &blobs)?;
    manifest_descriptor["mediaType"] = json!(MANIFEST);
    manifest_descriptor["annotations"] = json!({"org.opencontainers.image.ref.name": conf.repo_tag.rsplit_once(':').map_or("latest", |(_, tag)| tag)});
    fs::write(
        work.path().join("oci-layout"),
        b"{\"imageLayoutVersion\":\"1.0.0\"}\n",
    )?;
    fs::write(
        work.path().join("index.json"),
        serde_json::to_vec_pretty(
            &json!({"schemaVersion": 2, "mediaType": INDEX, "manifests": [manifest_descriptor]}),
        )?,
    )?;
    fs::write(wrix.join("manifest.json"), manifest_bytes)?;
    fs::write(
        wrix.join("config-digest"),
        format!(
            "{}\n",
            config_descriptor["digest"]
                .as_str()
                .ok_or(Error::Metadata)?
        ),
    )?;
    let descriptors: Vec<_> = layers
        .iter()
        .map(|layer| {
            let mut descriptor = layer.descriptor();
            descriptor["diff_id"] = json!(layer.digest.reference());
            descriptor
        })
        .collect();
    fs::write(
        wrix.join("descriptor-manifest.json"),
        serde_json::to_vec_pretty(
            &json!({"media_type": MANIFEST, "config": config_descriptor, "layers": descriptors}),
        )?,
    )?;
    fs::rename(work.path(), &options.output)?;
    Ok(())
}

#[cfg(test)]
mod test;
