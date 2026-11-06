//! Resolve contextual configuration tokens using declared native closure roots.

use std::{collections::BTreeSet, fs, path::Path};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// invalid native store path: {0}
    Path(String),
    /// configuration reference is outside its input closure: {0}
    OutsideClosure(String),
    /// invalid configuration references: {0}
    Json(#[from] serde_json::Error),
    /// configuration-root filesystem operation failed: {0}
    Io(#[from] std::io::Error),
}

/// A complete native store root or contextual token, without a subpath.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct StorePath(String);

impl StorePath {
    pub fn parse(value: String) -> Result<Self, Error> {
        let Some(suffix) = value.strip_prefix("/nix/store/") else {
            return Err(Error::Path(value));
        };
        let bytes = suffix.as_bytes();
        let valid = bytes.len() > 33
            && bytes[..32]
                .iter()
                .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(byte))
            && bytes[32] == b'-'
            && bytes[33..]
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-+._?=".contains(byte));
        if !valid {
            return Err(Error::Path(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

pub fn parse_references(bytes: &[u8]) -> Result<Vec<StorePath>, Error> {
    let raw: Vec<String> = serde_json::from_slice(bytes)?;
    raw.into_iter().map(StorePath::parse).collect()
}

pub fn parse_closure(text: &str) -> Result<BTreeSet<StorePath>, Error> {
    text.lines()
        .map(|line| StorePath::parse(line.to_owned()))
        .collect()
}

/// The native closure, not a guessed derivation name, supplies the real root.
pub fn resolve(
    candidates: &[StorePath],
    closure: &BTreeSet<StorePath>,
) -> Result<BTreeSet<StorePath>, Error> {
    candidates
        .iter()
        .map(|candidate| {
            closure
                .iter()
                .filter(|root| candidate.as_str().starts_with(root.as_str()))
                .max_by_key(|root| root.as_str().len())
                .cloned()
                .ok_or_else(|| Error::OutsideClosure(candidate.as_str().to_owned()))
        })
        .collect()
}

pub fn publish(
    candidates: &[StorePath],
    closure: &BTreeSet<StorePath>,
    sources: &[StorePath],
    output: &Path,
) -> Result<(), Error> {
    let mut roots = resolve(candidates, closure)?;
    for source in sources {
        // Exact opaque native input paths are authority. Do not follow symlinks
        // or inspect derivation metadata to invent a different root.
        fs::symlink_metadata(source.as_path())?;
        roots.insert(source.clone());
    }
    let parent = output.join("share/wrix");
    fs::create_dir_all(&parent)?;
    let destination = parent.join("image-inputs");
    fs::create_dir(&destination)?;
    for root in roots {
        let Some(name) = root.as_path().file_name() else {
            return Err(Error::Path(root.as_str().to_owned()));
        };
        std::os::unix::fs::symlink(root.as_path(), destination.join(name))?;
    }
    Ok(())
}

#[cfg(test)]
mod test;
