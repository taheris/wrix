//! Native Apple index identities are not OCI configuration identities.
use super::{
    Digest, Error, ImageId, ImageRef, Value, content_digest_id, run_required, run_required_output,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use wrix_core::nix_store::pool::System;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u16,
    index: Digest,
    config: Digest,
    system: System,
}

const fn system() -> Result<System, Error> {
    if cfg!(target_arch = "aarch64") {
        Ok(System::Aarch64Linux)
    } else if cfg!(target_arch = "x86_64") {
        Ok(System::X86_64Linux)
    } else {
        Err(Error::InvalidMetadata)
    }
}

fn directory() -> Result<PathBuf, Error> {
    let host = wrix_state::host::Host::discover().map_err(std::io::Error::other)?;
    Ok(host
        .roots()
        .state()
        .join("image")
        .join("apple-config")
        .join(system()?.as_str()))
}

fn index(value: &Value) -> Result<Digest, Error> {
    if value.as_array().map(Vec::len) != Some(1) {
        return Err(Error::InvalidMetadata);
    }
    let descriptor = value
        .pointer("/0/configuration/descriptor/digest")
        .and_then(Value::as_str)
        .ok_or(Error::InvalidMetadata)?;
    let index = Digest::parse(descriptor)?;
    let id = value
        .pointer("/0/id")
        .and_then(Value::as_str)
        .ok_or(Error::InvalidMetadata)?;
    if content_digest_id(&ImageId::parse(id)?)?.as_ref() != Some(&index) {
        return Err(Error::InvalidMetadata);
    }
    Ok(index)
}

fn read(root: &Path, expected: &Digest) -> Result<Option<Digest>, Error> {
    let path = root.join(expected.as_str().trim_start_matches("sha256:"));
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let parent = fs::symlink_metadata(root)?;
    let metadata = file.metadata()?;
    if !parent.is_dir()
        || parent.uid() != rustix::process::geteuid().as_raw()
        || parent.mode() & 0o777 != 0o700
        || !metadata.is_file()
        || metadata.uid() != parent.uid()
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > 1024
    {
        return Err(Error::InvalidMetadata);
    }
    let mut bytes = Vec::new();
    file.take(1025).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 {
        return Err(Error::InvalidMetadata);
    }
    let receipt: Receipt =
        serde_json::from_slice(&bytes).map_err(|source| Error::DescriptorJson { source })?;
    if receipt.version != 1 || receipt.index != *expected || receipt.system != system()? {
        return Err(Error::InvalidMetadata);
    }
    Ok(Some(receipt.config))
}

pub(super) fn cached(value: &Value) -> Result<Option<Digest>, Error> {
    read(&directory()?, &index(value)?)
}

/// Called only by image installation, never by passive inspection or preflight.
pub(super) fn attest(reference: &ImageRef, temporary: &Path) -> Result<(), Error> {
    let before =
        super::run_required_output("container", &["image", "inspect", reference.as_str()])?;
    let value: Value = serde_json::from_slice(&before.stdout)
        .map_err(|source| Error::DescriptorJson { source })?;
    if value.pointer("/0/configuration/descriptor").is_none() {
        return Ok(());
    }
    let index = index(&value)?;
    let root = directory()?;
    if read(&root, &index)?.is_some() {
        return Ok(());
    }
    let archive = temporary.join(format!(
        "attestation-{}.oci",
        index.as_str().trim_start_matches("sha256:")
    ));
    let system = system()?;
    let architecture = match system {
        System::Aarch64Linux => "arm64",
        System::X86_64Linux => "amd64",
    };
    run_required(
        "container",
        &[
            "image",
            "save",
            "--platform",
            &format!("linux/{architecture}"),
            "--output",
            &archive.display().to_string(),
            reference.as_str(),
        ],
    )?;
    let manifest = run_required_output(
        "skopeo",
        &[
            "--override-os",
            "linux",
            "--override-arch",
            architecture,
            "inspect",
            "--raw",
            &format!("oci-archive:{}", archive.display()),
        ],
    )?;
    let manifest: Value = serde_json::from_slice(&manifest.stdout)
        .map_err(|source| Error::DescriptorJson { source })?;
    let config = Digest::parse(
        manifest
            .pointer("/config/digest")
            .and_then(Value::as_str)
            .ok_or(Error::InvalidMetadata)?,
    )?;
    let after = super::run_required_output("container", &["image", "inspect", reference.as_str()])?;
    let value: Value =
        serde_json::from_slice(&after.stdout).map_err(|source| Error::DescriptorJson { source })?;
    if self::index(&value)? != index {
        return Err(Error::InvalidMetadata);
    }
    wrix_core::private_directory::prepare_with_parents(&root).map_err(std::io::Error::other)?;
    let mut file = tempfile::NamedTempFile::new_in(&root)?;
    file.write_all(
        &serde_json::to_vec(&Receipt {
            version: 1,
            index: index.clone(),
            config,
            system,
        })
        .map_err(|source| Error::DescriptorJson { source })?,
    )?;
    file.as_file().sync_all()?;
    file.persist(root.join(index.as_str().trim_start_matches("sha256:")))
        .map_err(|error| error.error)?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn reported_id(bytes: &[u8]) -> Result<ImageId, Error> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|source| Error::DescriptorJson { source })?;
    if value.as_array().map(Vec::len) != Some(1) {
        return Err(Error::InvalidMetadata);
    }
    if value.pointer("/0/configuration/descriptor").is_some() {
        index(&value)?;
    }
    Ok(ImageId::parse(
        value
            .pointer("/0/id")
            .and_then(Value::as_str)
            .ok_or(Error::InvalidMetadata)?,
    )?)
}
fn same_id(actual: &ImageId, expected: &ImageId) -> Result<bool, Error> {
    match (content_digest_id(actual)?, content_digest_id(expected)?) {
        (Some(actual), Some(expected)) => Ok(actual == expected),
        _ => Ok(actual == expected),
    }
}
pub(super) fn require_id(bytes: &[u8], expected: &ImageId) -> Result<(), Error> {
    if !same_id(&reported_id(bytes)?, expected)? {
        return Err(Error::InvalidMetadata);
    }
    Ok(())
}

/// Apple selectors are references, not the hexadecimal IDs reported by inspection.
pub(super) fn reference(expected: &ImageId) -> Result<String, Error> {
    let images = run_required_output("container", &["image", "list", "--format", "json"])?;
    for row in super::container_image_rows(&images.stdout)? {
        let inspection =
            run_required_output("container", &["image", "inspect", row.target.as_str()])?;
        if same_id(&reported_id(&inspection.stdout)?, expected)? {
            return Ok(row.target.as_str().to_owned());
        }
    }
    Err(Error::MissingImageId {
        target: expected.as_str().to_owned(),
    })
}

#[cfg(test)]
mod test;
