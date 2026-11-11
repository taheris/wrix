//! Ownership and retained private state from the emitted final image layer.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read},
    path::Path,
};

use serde_json::Value;

use super::{Image, archive, bounded, path};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// emitted permission inspection failed: {0}
    Io(#[from] io::Error),
    /// emitted config is invalid: {0}
    Json(#[from] serde_json::Error),
    /// emitted private Nix contract failed: {0}
    Contract(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
struct Owner {
    uid: u64,
    gid: u64,
}

#[derive(Debug)]
struct Entry {
    owner: Owner,
    mode: u32,
    kind: tar::EntryType,
    link: Option<Vec<u8>>,
}

impl Entry {
    fn regular(&self) -> bool {
        matches!(self.kind.as_byte(), 0 | b'0' | b'7' | b'S')
    }
}

const EXPECTED: &[(&str, u64, u32)] = &[
    ("nix", 0, 0o755),
    ("nix/var", 0, 0o755),
    ("nix/var/nix", 0, 0o755),
    ("nix/store", 1000, 0o755),
    ("nix/var/nix/db", 1000, 0o700),
    ("nix/var/nix/temproots", 1000, 0o700),
    ("nix/var/nix/builds", 1000, 0o755),
    ("nix/var/nix/gc.lock", 1000, 0o600),
    ("nix/var/nix/profiles", 0, 0o755),
    ("nix/var/nix/profiles/per-user", 0, 0o755),
    ("nix/var/nix/profiles/per-user/wrix", 1000, 0o755),
    ("nix/var/nix/gcroots", 0, 0o755),
    ("nix/var/nix/gcroots/per-user", 0, 0o755),
    ("nix/var/nix/gcroots/wrix-image", 0, 0o755),
    ("nix/var/nix/gcroots/per-user/wrix", 1000, 0o755),
    ("nix/var/nix/gcroots/auto", 0, 0o1777),
    ("nix/var/log/nix/drvs", 1000, 0o755),
];
const MARKER: &[u8] = b"etc/wrix/private-nix-version";
const ROOTS: &[u8] = b"etc/wrix/store-roots";
const PREFIX: &[u8] = b"nix/var/nix/gcroots/wrix-image/";

const fn require(condition: bool, message: &'static str) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(Error::Contract(message))
    }
}

fn normalized(raw: &[u8]) -> Vec<u8> {
    let name = raw.strip_prefix(b"./").unwrap_or(raw);
    let start = name
        .iter()
        .position(|byte| *byte != b'/')
        .unwrap_or(name.len());
    let end = name
        .iter()
        .rposition(|byte| *byte != b'/')
        .map_or(start, |index| index + 1);
    name[start..end].to_vec()
}

fn final_layer(image: &Image) -> Result<Box<dyn Read>, Error> {
    let last = image
        .layers
        .last()
        .ok_or(Error::Contract("image has no final layer"))?;
    let (mut reader, offset, size): (Box<dyn Read>, _, _) =
        if let Some((source, locations)) = &image.source {
            let &(offset, size) = locations
                .get(last)
                .ok_or(Error::Contract("final archive layer location missing"))?;
            (archive::open(source)?, offset, size)
        } else {
            let file = File::open(image.root.join(path(last)?))?;
            let size = file.metadata()?.len();
            (Box::new(file), 0, size)
        };
    require(
        archive::drain(&mut (&mut reader).take(offset))? == offset,
        "truncated outer archive",
    )?;
    Ok(Box::new(reader.take(size)))
}

fn inspect(image: &Image) -> Result<(), Error> {
    let mut layer = tar::Archive::new(final_layer(image)?);
    let mut entries = BTreeMap::new();
    let mut marker = None;
    let mut roots = None;
    for entry in layer.entries()? {
        let mut entry = entry?;
        let name = normalized(&entry.path_bytes());
        let header = entry.header();
        let size = header.size()?;
        let metadata = Entry {
            owner: Owner {
                uid: header.uid()?,
                gid: header.gid()?,
            },
            mode: header.mode()?,
            kind: header.entry_type(),
            link: entry.link_name_bytes().map(std::borrow::Cow::into_owned),
        };
        if name == MARKER {
            marker = Some(bounded(&mut entry, size)?);
        } else if name == ROOTS {
            roots = Some(bounded(&mut entry, size)?);
        }
        entries.insert(name, metadata);
    }
    archive::drain(&mut layer.into_inner())?;
    check(&entries, marker.as_deref(), roots.as_deref(), &image.root)
}

fn member<'a>(entries: &'a BTreeMap<Vec<u8>, Entry>, name: &[u8]) -> Result<&'a Entry, Error> {
    entries
        .get(name)
        .ok_or(Error::Contract("required final-layer member absent"))
}

fn check(
    entries: &BTreeMap<Vec<u8>, Entry>,
    marker: Option<&[u8]>,
    roots: Option<&[u8]>,
    image_root: &Path,
) -> Result<(), Error> {
    for &(name, uid, mode) in EXPECTED {
        let entry = member(entries, name.as_bytes())?;
        require(
            entry.owner == Owner { uid, gid: uid } && entry.mode == mode,
            "owner or mode differs",
        )?;
        require(
            if name.ends_with("gc.lock") {
                entry.regular()
            } else {
                entry.kind.is_dir()
            },
            "required member type differs",
        )?;
    }
    for forbidden in [
        "homeless-shelter",
        "nix/var/nix/daemon-socket/socket",
        "nix/var/nix/db/reserved",
        "etc/wrix/image-policy.json",
        "lib/libfakeuid.so",
    ] {
        require(
            !entries.contains_key(forbidden.as_bytes()),
            "forbidden final-layer member present",
        )?;
    }
    member(entries, b"nix/var/nix/db/db.sqlite")?;
    require(
        member(entries, MARKER)?.regular(),
        "private marker is not regular",
    )?;
    require(
        marker == Some(b"1\n".as_slice()),
        "private marker bytes differ",
    )?;
    let roots = std::str::from_utf8(roots.ok_or(Error::Contract("root manifest absent"))?)
        .map_err(io::Error::other)?;
    let roots: Vec<_> = roots.lines().collect();
    require(!roots.is_empty(), "root manifest empty")?;
    let mut expected = BTreeSet::new();
    for root in roots {
        let basename = root
            .split('/')
            .rev()
            .find(|component| !component.is_empty() && *component != ".")
            .ok_or(Error::Contract("root manifest basename absent"))?;
        let mut name = PREFIX.to_vec();
        name.extend_from_slice(basename.as_bytes());
        let link = member(entries, &name)?;
        require(
            link.kind.is_symlink() && link.link.as_deref() == Some(root.as_bytes()),
            "manifest GC root is not a direct matching symlink",
        )?;
        require(
            link.owner == Owner { uid: 0, gid: 0 },
            "manifest GC root is not root-owned",
        )?;
        expected.insert(name);
    }
    let actual: BTreeSet<_> = entries
        .keys()
        .filter(|name| name.starts_with(PREFIX))
        .cloned()
        .collect();
    require(actual == expected, "GC-root members differ from manifest")?;
    let mut file = File::open(image_root.join("config.json"))?;
    let size = file.metadata()?.len();
    let config: Value = serde_json::from_slice(&bounded(&mut file, size)?)?;
    let config = config
        .get("config")
        .ok_or(Error::Contract("config object absent"))?;
    let env = config
        .get("Env")
        .and_then(Value::as_array)
        .ok_or(Error::Contract("config environment absent"))?;
    for item in env {
        let (name, _) = item
            .as_str()
            .and_then(|value| value.split_once('='))
            .ok_or(Error::Contract("config environment entry invalid"))?;
        require(name != "NIX_REMOTE", "NIX_REMOTE present")?;
    }
    require(
        env.contains(&Value::String("IS_SANDBOX=1".to_owned())),
        "sandbox environment absent",
    )?;
    let labels = config
        .get("Labels")
        .and_then(Value::as_object)
        .ok_or(Error::Contract("config labels absent"))?;
    require(
        labels.get("wrix.nix.private") == Some(&Value::String("1".to_owned())),
        "private label differs",
    )?;
    require(
        !labels.contains_key("wrix.image.policy"),
        "legacy policy label present",
    )
}

#[derive(Clone, Copy)]
pub enum Format {
    Oci,
    Archive,
}

pub fn run(source: &Path, format: Format) -> Result<(), Error> {
    require(
        match format {
            Format::Oci => source.is_dir(),
            Format::Archive => {
                !source.is_dir()
                    && source
                        .extension()
                        .is_none_or(|extension| extension != "json")
            }
        },
        "source does not match requested format",
    )?;
    let temporary = tempfile::tempdir()?;
    let image = Image::prepare(source, &temporary.path().join("image"))?;
    inspect(&image)
}

#[cfg(test)]
mod test;
