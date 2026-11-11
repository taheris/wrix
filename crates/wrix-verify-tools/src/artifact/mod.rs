//! Verified image inspection with header-only queries and bounded payload access.

pub mod archive;
pub mod image;
pub mod text;

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error, displaydoc::Display, PartialEq, Eq)]
pub enum Error {
    /// unsafe archive member: {0}
    UnsafeName(String),
    /// cyclic archive hardlink: {0}
    HardlinkCycle(String),
    /// missing archive hardlink target: {0}
    MissingTarget(String),
    /// not a regular file: {0}
    NotRegular(String),
}

/// A normalized relative POSIX archive path, never a host filesystem path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Name(String);

impl Name {
    pub fn parse(raw: &str) -> Result<Self, Error> {
        let mut parts = Vec::new();
        for part in raw.split('/') {
            match part {
                ".." => return Err(Error::UnsafeName(raw.to_owned())),
                "" | "." => {}
                _ => parts.push(part),
            }
        }
        Ok(Self(if parts.is_empty() {
            ".".to_owned()
        } else {
            parts.join("/")
        }))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn ancestors(&self) -> impl Iterator<Item = &str> {
        self.0
            .match_indices('/')
            .rev()
            .map(|(end, _)| &self.0[..end])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Regular,
    Hardlink(Name),
    Symlink(String),
    Other(u8),
}

/// Header data sufficient for lookup without reading any archive payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub mode: u32,
}

/// A single layer's emitted headers, independent of visibility in the final image.
#[derive(Debug)]
pub struct Layer {
    members: BTreeMap<Name, Entry>,
}

impl Layer {
    pub fn parse(entries: impl IntoIterator<Item = (String, Entry)>) -> Result<Self, Error> {
        let mut members = BTreeMap::new();
        for (raw, entry) in entries {
            let name = Name::parse(&raw)?;
            members.insert(name, entry);
        }
        Ok(Self { members })
    }

    fn hides(&self, name: &Name) -> bool {
        std::iter::once(name.as_str())
            .chain(name.ancestors())
            .any(|path| {
                let whiteout = match path.rsplit_once('/') {
                    Some((parent, leaf)) => format!("{parent}/.wh.{leaf}"),
                    None => format!(".wh.{path}"),
                };
                self.members.contains_key(&Name(whiteout))
            })
            || name
                .ancestors()
                .map(|parent| format!("{parent}/.wh..wh..opq"))
                .chain(std::iter::once(".wh..wh..opq".to_owned()))
                .any(|opaque| self.members.contains_key(&Name(opaque)))
    }
}

/// The caller supplies only the selected tiers, in bottom-to-top order.
#[derive(Debug)]
pub struct Selection<'a> {
    layers: Vec<&'a Layer>,
}

pub struct Found<'a> {
    pub layer: usize,
    pub name: &'a Name,
    pub entry: &'a Entry,
    members: &'a BTreeMap<Name, Entry>,
}

impl<'a> Selection<'a> {
    pub fn new(layers: impl IntoIterator<Item = &'a Layer>) -> Self {
        Self {
            layers: layers.into_iter().collect(),
        }
    }

    pub fn lookup(&self, name: &Name) -> Option<Found<'a>> {
        for (index, layer) in self.layers.iter().enumerate().rev() {
            if let Some((name, entry)) = layer.members.get_key_value(name) {
                return Some(Found {
                    layer: index,
                    name,
                    entry,
                    members: &layer.members,
                });
            }
            if layer.hides(name) {
                return None;
            }
        }
        None
    }

    /// Emitted names include lower-layer entries hidden by later whiteouts.
    pub fn names(&self) -> BTreeSet<&'a Name> {
        self.layers
            .iter()
            .flat_map(|layer| layer.members.keys())
            .collect()
    }

    pub fn store_paths(&self) -> BTreeSet<String> {
        self.names()
            .into_iter()
            .filter_map(|name| store_path(name.as_str()))
            .collect()
    }
}

impl<'a> Found<'a> {
    /// Hardlinks resolve only within the layer that emitted the link.
    pub fn regular(&self) -> Result<(&'a Name, &'a Entry), Error> {
        let mut visited = BTreeSet::new();
        let mut name = self.name;
        let mut entry = self.entry;
        while let Kind::Hardlink(target) = &entry.kind {
            if !visited.insert(target) {
                return Err(Error::HardlinkCycle(target.0.clone()));
            }
            (name, entry) = self
                .members
                .get_key_value(target)
                .ok_or_else(|| Error::MissingTarget(target.0.clone()))?;
        }
        if entry.kind != Kind::Regular {
            return Err(Error::NotRegular(name.0.clone()));
        }
        Ok((name, entry))
    }
}

fn store_path(name: &str) -> Option<String> {
    let parts: Vec<_> = name.split('/').collect();
    parts.windows(3).find_map(|parts| {
        let root = parts[2].as_bytes();
        (parts[0] == "nix"
            && parts[1] == "store"
            && root.len() > 33
            && root[32] == b'-'
            && root[..32]
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()))
        .then(|| format!("/nix/store/{}", parts[2]))
    })
}

#[cfg(test)]
mod test;
