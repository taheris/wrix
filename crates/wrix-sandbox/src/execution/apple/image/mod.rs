//! Protected installation receipts prove config digest separately from Apple's immutable index descriptor.
#[cfg(test)]
mod test;
use super::{Error, Path, PathBuf, Value, required};
use crate::image::Digest;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    process::Command,
};
use wrix_core::nix_store::pool::System;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u16,
    index: Digest,
    config: Digest,
    system: System,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Proof {
    pub index: Digest,
    pub reference: crate::image::ImageRef,
    pub architecture: String,
    pub executable: PathBuf,
    expected: Digest,
    seed: PathBuf,
}
const fn system() -> Result<(System, &'static str), Error> {
    if cfg!(target_arch = "aarch64") {
        Ok((System::Aarch64Linux, "arm64"))
    } else if cfg!(target_arch = "x86_64") {
        Ok((System::X86_64Linux, "amd64"))
    } else {
        Err(Error::Delivery)
    }
}
fn receipt(index: &Digest, expected: &Digest) -> Result<(), Error> {
    let host = wrix_state::host::Host::discover()?;
    let (system, _) = system()?;
    let root = host
        .roots()
        .state()
        .join("image/apple-config")
        .join(system.as_str());
    let parent = fs::symlink_metadata(&root)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(index.as_str().trim_start_matches("sha256:")))?;
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
        return Err(Error::Delivery);
    }
    let mut bytes = Vec::new();
    file.take(1025).read_to_end(&mut bytes)?;
    let receipt: Receipt = serde_json::from_slice(&bytes)?;
    if receipt.version != 1
        || receipt.index != *index
        || receipt.config != *expected
        || receipt.system != system
    {
        return Err(Error::Delivery);
    }
    Ok(())
}
fn native(bytes: &[u8], seed: &Path) -> Result<(Digest, &'static str, PathBuf), Error> {
    let values: Vec<Value> = serde_json::from_slice(bytes)?;
    if values.len() != 1 {
        return Err(Error::Delivery);
    }
    let value = &values[0];
    let index = Digest::parse(
        value
            .pointer("/configuration/descriptor/digest")
            .and_then(Value::as_str)
            .ok_or(Error::Delivery)?,
    )
    .map_err(|_| Error::Delivery)?;
    if value["id"].as_str() != Some(index.as_str().trim_start_matches("sha256:")) {
        return Err(Error::Delivery);
    }
    let (_, architecture) = system()?;
    let variants = value["variants"].as_array().ok_or(Error::Delivery)?;
    let mut variants = variants.iter().filter(|variant| {
        variant["platform"]["os"] == "linux" && variant["platform"]["architecture"] == architecture
    });
    let variant = variants.next().ok_or(Error::Delivery)?;
    if variants.next().is_some()
        || variant["config"]["os"] != "linux"
        || variant["config"]["architecture"] != architecture
    {
        return Err(Error::Delivery);
    }
    let labels = &variant["config"]["config"]["Labels"];
    if labels["wrix.nix.apple.bridge.version"] != "1"
        || labels["wrix.nix.shared"] != "1"
        || labels["wrix.nix.client"] != "1"
    {
        return Err(Error::Delivery);
    }
    let executable = labels["wrix.nix.apple.bridge.executable"]
        .as_str()
        .ok_or(Error::Delivery)?;
    let root = executable
        .strip_suffix("/bin/wrix-apple-nix-bridge")
        .ok_or(Error::Delivery)?;
    let base = root
        .strip_prefix("/nix/store/")
        .and_then(|base| base.split_once('-'))
        .ok_or(Error::Delivery)?;
    if base.0.len() != 32
        || !base
            .0
            .bytes()
            .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&byte))
        || base.1.is_empty()
        || !base
            .1
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+-._?=".contains(&byte))
    {
        return Err(Error::Delivery);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(seed)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > 16_777_216 {
        return Err(Error::Delivery);
    }
    let mut closure = String::new();
    file.take(16_777_217).read_to_string(&mut closure)?;
    if !closure.lines().any(|line| line == root) {
        return Err(Error::Delivery);
    }
    Ok((index, architecture, executable.into()))
}
impl Proof {
    pub fn inspect(
        runtime: &Command,
        reference: &OsStr,
        expected: &Digest,
        seed: &Path,
    ) -> Result<Self, Error> {
        let bytes = required(runtime, &["image".as_ref(), "inspect".as_ref(), reference])?;
        let (index, architecture, executable) = native(&bytes, seed)?;
        let values: Vec<Value> = serde_json::from_slice(&bytes)?;
        let reference = crate::image::ImageRef::parse(
            values[0]["configuration"]["name"]
                .as_str()
                .ok_or(Error::Delivery)?,
        )
        .map_err(|_| Error::Delivery)?;
        receipt(&index, expected)?;
        Ok(Self {
            index,
            reference,
            architecture: architecture.into(),
            executable,
            expected: expected.clone(),
            seed: seed.to_path_buf(),
        })
    }
    pub fn recheck(&self, runtime: &Command) -> Result<(), Error> {
        let current = Self::inspect(
            runtime,
            self.reference.as_str().as_ref(),
            &self.expected,
            &self.seed,
        )?;
        if current.index != self.index
            || current.executable != self.executable
            || current.architecture != self.architecture
        {
            return Err(Error::Delivery);
        }
        Ok(())
    }
}
