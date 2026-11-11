//! Image inspection records retain headers and reference original payloads.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::{
    ffi::OsStrExt,
    fs::{PermissionsExt, symlink},
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use super::{
    archive::{self, Header},
    text::{Json, Text},
};

pub mod command;
mod pattern;
pub mod shared;

#[cfg(test)]
mod test;

type Members = BTreeMap<Text, Header>;
type ArchiveSource = (PathBuf, BTreeMap<Text, (u64, u64)>);

fn object(values: impl IntoIterator<Item = (&'static str, Json)>) -> Json {
    Json::Object(
        values
            .into_iter()
            .map(|(key, value)| (Text::from_utf8(key), value))
            .collect(),
    )
}

fn text(value: &str) -> Json {
    Json::String(Text::from_utf8(value))
}
fn number(value: u64) -> Json {
    Json::Number(value.into())
}
fn path_text(path: &Path) -> Text {
    Text::from_bytes(path.as_os_str().as_bytes())
}
fn path(value: &Text) -> io::Result<PathBuf> {
    let bytes = value.bytes()?;
    if bytes.contains(&0) {
        return Err(io::Error::other("NUL in filesystem path"));
    }
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
}
fn read_json(path: &Path) -> io::Result<Json> {
    Json::parse(&fs::read(path)?)
}
fn write_json(path: &Path, value: &Json) -> io::Result<()> {
    fs::write(path, value.encode())
}
fn bounded(reader: &mut dyn Read, size: u64) -> io::Result<Vec<u8>> {
    if size > 16 * 1024 * 1024 {
        return Err(io::Error::other("invalid or oversized image metadata"));
    }
    let mut data = Vec::new();
    reader.take(size + 1).read_to_end(&mut data)?;
    if u64::try_from(data.len()).map_err(io::Error::other)? != size {
        return Err(io::Error::other("image metadata size mismatch"));
    }
    Ok(data)
}
fn metadata_file(path: &Path) -> io::Result<Json> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    Json::parse(&bounded(&mut file, size)?)
}

struct HashReader<R> {
    reader: R,
    hash: Sha256,
}
impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.reader.read(bytes)?;
        self.hash.update(&bytes[..count]);
        Ok(count)
    }
}

fn blob(layout: &Path, descriptor: &Json) -> io::Result<PathBuf> {
    let digest = descriptor.get("digest")?.text()?.unicode()?;
    let hex = digest
        .strip_prefix("sha256:")
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| io::Error::other("unsupported blob digest"))?;
    let filename = layout.join("blobs/sha256").join(hex);
    if filename.metadata()?.len() != descriptor.get("size")?.integer()? {
        return Err(io::Error::other("blob size mismatch"));
    }
    let mut reader = HashReader {
        reader: File::open(&filename)?,
        hash: Sha256::new(),
    };
    archive::drain(&mut reader)?;
    if format!("{:x}", reader.hash.finalize()) != hex {
        return Err(io::Error::other("blob digest mismatch"));
    }
    Ok(filename)
}

fn header_json(header: &Header) -> Json {
    object([
        ("name", Json::String(header.name.clone())),
        ("type", Json::String(Text::from_bytes(&[header.kind]))),
        ("mode", number(u64::from(header.mode))),
        ("size", number(header.size)),
        ("offset", number(header.offset)),
        ("offset_data", number(header.offset_data)),
        ("linkname", Json::String(header.link.clone())),
        (
            "sparse",
            header.sparse.as_ref().map_or(Json::Null, |map| {
                Json::Array(
                    map.iter()
                        .map(|&(start, size)| Json::Array(vec![number(start), number(size)]))
                        .collect(),
                )
            }),
        ),
    ])
}

fn parse_header(value: &Json) -> io::Result<Header> {
    let kind = value.get("type")?.text()?.bytes()?;
    if kind.len() != 1 {
        return Err(io::Error::other("invalid tar entry type"));
    }
    let sparse = match value.get("sparse")? {
        Json::Null => None,
        value => Some(
            value
                .array()?
                .iter()
                .map(|pair| {
                    let pair = pair.array()?;
                    if pair.len() != 2 {
                        return Err(io::Error::other("invalid sparse extent"));
                    }
                    Ok((pair[0].integer()?, pair[1].integer()?))
                })
                .collect::<io::Result<_>>()?,
        ),
    };
    Ok(Header {
        name: value.get("name")?.text()?.clone(),
        kind: kind[0],
        mode: u32::try_from(value.get("mode")?.integer()?).map_err(io::Error::other)?,
        size: value.get("size")?.integer()?,
        offset: value.get("offset")?.integer()?,
        offset_data: value.get("offset_data")?.integer()?,
        link: value.get("linkname")?.text()?.clone(),
        sparse,
    })
}

#[derive(Debug)]
pub struct Image {
    root: PathBuf,
    pub layers: Vec<Text>,
    members: BTreeMap<Text, Members>,
    digests: BTreeMap<Text, Json>,
    source: Option<ArchiveSource>,
}

impl Image {
    pub fn prepare(source: &Path, output: &Path) -> io::Result<Self> {
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir(output)?;
        let mut image = Self {
            root: output.to_owned(),
            layers: Vec::new(),
            members: BTreeMap::new(),
            digests: BTreeMap::new(),
            source: None,
        };
        if source.is_dir()
            || source
                .extension()
                .is_some_and(|extension| extension == "json")
        {
            image.oci(source)?;
        } else {
            image.docker(source)?;
        }
        image.save()?;
        Ok(image)
    }

    fn oci(&mut self, source: &Path) -> io::Result<()> {
        let layout = if source.is_dir() {
            source.to_owned()
        } else {
            path(metadata_file(source)?.get("oci_layout")?.text()?)?
        };
        let index = metadata_file(&layout.join("index.json"))?;
        let manifests = index.get("manifests")?.array()?;
        if manifests.len() != 1 {
            return Err(io::Error::other(
                "image inspection requires exactly one OCI manifest",
            ));
        }
        let manifest = metadata_file(&blob(&layout, &manifests[0])?)?;
        let config = blob(&layout, manifest.get("config")?)?;
        if config.metadata()?.len() > 16 * 1024 * 1024 {
            return Err(io::Error::other("invalid or oversized image metadata"));
        }
        symlink(config, self.root.join("config.json"))?;
        symlink(layout.join("blobs"), self.root.join("blobs"))?;
        for layer in manifest.get("layers")?.array()? {
            let filename = blob(&layout, layer)?;
            if layer.get("mediaType")?.text()?
                != &Text::from_utf8("application/vnd.oci.image.layer.v1.tar")
            {
                return Err(io::Error::other(
                    "image inspection requires uncompressed OCI layers",
                ));
            }
            if filename.metadata()?.len() != layer.get("size")?.integer()? {
                return Err(io::Error::other("layer size mismatch"));
            }
            let name = path_text(filename.strip_prefix(&layout).map_err(io::Error::other)?);
            self.layers.push(name.clone());
            self.digests
                .insert(name.clone(), layer.get("digest")?.clone());
            self.members
                .insert(name, archive::index(File::open(filename)?)?);
        }
        Ok(())
    }

    fn docker(&mut self, source: &Path) -> io::Result<()> {
        let executable = match rustix::fs::access(source, rustix::fs::Access::EXEC_OK) {
            Ok(()) => true,
            Err(rustix::io::Errno::ACCESS) => false,
            Err(error) => return Err(error.into()),
        };
        let source = if executable {
            let snapshot = self.root.join("archive.tar");
            let result = Command::new(source)
                .stdout(Stdio::from(File::create(&snapshot)?))
                .status();
            match result {
                Ok(status) if status.success() => {}
                result => {
                    fs::remove_file(&snapshot)?;
                    return Err(io::Error::other(format!(
                        "image stream producer failed: {result:?}"
                    )));
                }
            }
            snapshot
        } else {
            source.to_owned()
        }
        .canonicalize()?;
        let mut headers = BTreeSet::new();
        let mut manifest = None;
        archive::scan(archive::open(&source)?, |header, payload| {
            let name = header.name.normalized()?;
            if !headers.insert(name.clone()) {
                return Err(io::Error::other("duplicate Docker archive member"));
            }
            if name == Text::from_utf8("manifest.json") {
                if !header.regular() {
                    return Err(io::Error::other("invalid or oversized image metadata"));
                }
                manifest = Some(Json::parse(&bounded(payload, header.size)?)?);
            }
            Ok(())
        })?;
        let manifest = manifest.ok_or_else(|| {
            io::Error::other("image inspection requires exactly one Docker manifest")
        })?;
        let manifests = manifest.array()?;
        if manifests.len() != 1 {
            return Err(io::Error::other(
                "image inspection requires exactly one Docker manifest",
            ));
        }
        let config = manifests[0].get("Config")?.text()?.normalized()?;
        self.layers = manifests[0]
            .get("Layers")?
            .array()?
            .iter()
            .map(|value| value.text()?.normalized())
            .collect::<io::Result<_>>()?;
        let layers: BTreeSet<_> = self.layers.iter().cloned().collect();
        if layers.len() != self.layers.len() {
            return Err(io::Error::other("duplicate Docker image layer"));
        }
        if !headers.contains(&config)
            || layers.contains(&config)
            || config == Text::from_utf8("manifest.json")
        {
            return Err(io::Error::other("invalid Docker config member"));
        }
        let mut locations = BTreeMap::new();
        archive::scan(archive::open(&source)?, |header, payload| {
            let name = header.name.normalized()?;
            if name == config {
                if !header.regular() {
                    return Err(io::Error::other("invalid or oversized image metadata"));
                }
                fs::write(
                    self.root.join("config.json"),
                    bounded(payload, header.size)?,
                )?;
            }
            if layers.contains(&name) {
                if !header.regular() || header.sparse.is_some() {
                    return Err(io::Error::other("not a regular Docker layer"));
                }
                let mut reader = HashReader {
                    reader: payload,
                    hash: Sha256::new(),
                };
                self.members
                    .insert(name.clone(), archive::index(&mut reader)?);
                self.digests.insert(
                    name.clone(),
                    text(&format!("sha256:{:x}", reader.hash.finalize())),
                );
                locations.insert(name, (header.offset_data, header.size));
            }
            Ok(())
        })?;
        if self.members.keys().collect::<BTreeSet<_>>() != layers.iter().collect() {
            return Err(io::Error::other("missing Docker image layer"));
        }
        self.source = Some((source, locations));
        Ok(())
    }

    fn save(&self) -> io::Result<()> {
        write_json(
            &self.root.join("manifest.json"),
            &Json::Array(vec![object([
                ("Config", text("config.json")),
                (
                    "Layers",
                    Json::Array(self.layers.iter().cloned().map(Json::String).collect()),
                ),
            ])]),
        )?;
        write_json(
            &self.root.join("members.json"),
            &Json::Object(
                self.members
                    .iter()
                    .map(|(layer, members)| {
                        (
                            layer.clone(),
                            Json::Object(
                                members
                                    .iter()
                                    .map(|(name, header)| (name.clone(), header_json(header)))
                                    .collect(),
                            ),
                        )
                    })
                    .collect(),
            ),
        )?;
        write_json(
            &self.root.join("layer-digests.json"),
            &Json::Object(self.digests.clone()),
        )?;
        if let Some((source, locations)) = &self.source {
            write_json(
                &self.root.join("archive.json"),
                &object([
                    ("source", Json::String(path_text(source))),
                    (
                        "layers",
                        Json::Object(
                            locations
                                .iter()
                                .map(|(layer, &(offset, size))| {
                                    (
                                        layer.clone(),
                                        object([
                                            ("offset", number(offset)),
                                            ("size", number(size)),
                                        ]),
                                    )
                                })
                                .collect(),
                        ),
                    ),
                ]),
            )?;
        }
        Ok(())
    }

    pub fn load(root: &Path, layers_file: &Path) -> io::Result<Self> {
        let layers = fs::read_to_string(layers_file)?
            .lines()
            .filter(|line| !line.is_empty())
            .map(Text::from_utf8)
            .collect();
        Self::with_layers(root, layers)
    }

    pub fn prepared(root: &Path) -> io::Result<Self> {
        let manifest = metadata_file(&root.join("manifest.json"))?;
        let [manifest] = manifest.array()? else {
            return Err(io::Error::other("expected one prepared image manifest"));
        };
        if manifest.get("Config")?.text()? != &Text::from_utf8("config.json") {
            return Err(io::Error::other("invalid prepared config path"));
        }
        let layers = manifest
            .get("Layers")?
            .array()?
            .iter()
            .map(|name| name.text().cloned())
            .collect::<io::Result<_>>()?;
        Self::with_layers(root, layers)
    }

    fn with_layers(root: &Path, layers: Vec<Text>) -> io::Result<Self> {
        let Json::Object(encoded) = read_json(&root.join("members.json"))? else {
            return Err(io::Error::other("invalid members index"));
        };
        let members = encoded
            .into_iter()
            .map(|(layer, values)| {
                layer.normalized()?;
                let Json::Object(values) = values else {
                    return Err(io::Error::other("invalid layer index"));
                };
                let entries = values
                    .into_iter()
                    .map(|(name, value)| Ok((name.normalized()?, parse_header(&value)?)))
                    .collect::<io::Result<_>>()?;
                Ok((layer, entries))
            })
            .collect::<io::Result<_>>()?;
        let Json::Object(digests) = read_json(&root.join("layer-digests.json"))? else {
            return Err(io::Error::other("invalid layer digest index"));
        };
        let source = if root.join("archive.json").exists() {
            let value = read_json(&root.join("archive.json"))?;
            let Json::Object(locations) = value.get("layers")? else {
                return Err(io::Error::other("invalid layer locations"));
            };
            Some((
                path(value.get("source")?.text()?)?,
                locations
                    .iter()
                    .map(|(layer, location)| {
                        Ok((
                            layer.clone(),
                            (
                                location.get("offset")?.integer()?,
                                location.get("size")?.integer()?,
                            ),
                        ))
                    })
                    .collect::<io::Result<_>>()?,
            ))
        } else {
            None
        };
        let image = Self {
            root: root.to_owned(),
            layers,
            members,
            digests,
            source,
        };
        for layer in &image.layers {
            image.layer(layer)?;
        }
        Ok(image)
    }

    fn layer(&self, layer: &Text) -> io::Result<&Members> {
        self.members
            .get(layer)
            .ok_or_else(|| io::Error::other("selected layer missing from index"))
    }

    pub fn names(&self) -> io::Result<BTreeSet<&Text>> {
        let mut names = BTreeSet::new();
        for layer in &self.layers {
            names.extend(self.layer(layer)?.keys());
        }
        Ok(names)
    }

    pub fn lookup(&self, raw: &Text) -> io::Result<Option<(&Text, &Header)>> {
        let name = raw.normalized()?;
        let bytes = name.bytes()?;
        let mut paths = vec![bytes.as_slice()];
        for (position, byte) in bytes.iter().enumerate().rev() {
            if *byte == b'/' {
                paths.push(&bytes[..position]);
            }
        }
        for layer in self.layers.iter().rev() {
            let members = self.layer(layer)?;
            if let Some(header) = members.get(&name) {
                return Ok(Some((layer, header)));
            }
            for (index, candidate) in paths.iter().enumerate() {
                let leaf = candidate
                    .iter()
                    .rposition(|byte| *byte == b'/')
                    .map_or(0, |position| position + 1);
                let mut whiteout = candidate[..leaf].to_vec();
                whiteout.extend_from_slice(b".wh.");
                whiteout.extend_from_slice(&candidate[leaf..]);
                if members.contains_key(&Text::from_bytes(&whiteout)) {
                    return Ok(None);
                }
                if index != 0 {
                    let mut opaque = candidate.to_vec();
                    opaque.extend_from_slice(b"/.wh..wh..opq");
                    if members.contains_key(&Text::from_bytes(&opaque)) {
                        return Ok(None);
                    }
                }
            }
            if members.contains_key(&Text::from_utf8(".wh..wh..opq")) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn extract(&self, raw: &Text, output: &Path) -> io::Result<bool> {
        let Some((layer, mut header)) = self.lookup(raw)? else {
            return Ok(false);
        };
        let members = self.layer(layer)?;
        let mut visited = BTreeSet::new();
        while header.kind == b'1' {
            let target = header.link.normalized()?;
            if !visited.insert(target.clone()) {
                return Err(io::Error::other("cyclic archive hardlink"));
            }
            header = members
                .get(&target)
                .ok_or_else(|| io::Error::other("missing archive hardlink target"))?;
        }
        if !header.regular() {
            return Err(io::Error::other("not a regular file"));
        }
        let stored_size = if let Some(map) = &header.sparse {
            let mut end = 0;
            let mut total = 0_u64;
            for &(start, size) in map {
                if size == 0 {
                    continue;
                }
                let next = start
                    .checked_add(size)
                    .ok_or_else(|| io::Error::other("sparse extent overflow"))?;
                if start < end || next > header.size {
                    return Err(io::Error::other("invalid sparse extent"));
                }
                end = next;
                total = total
                    .checked_add(size)
                    .ok_or_else(|| io::Error::other("sparse length overflow"))?;
            }
            total
        } else {
            header.size
        };
        let (source, offset, bound) = if let Some((source, locations)) = &self.source {
            let &(base, size) = locations
                .get(layer)
                .ok_or_else(|| io::Error::other("missing Docker layer location"))?;
            (
                source.clone(),
                base.checked_add(header.offset_data)
                    .ok_or_else(|| io::Error::other("archive offset overflow"))?,
                size,
            )
        } else {
            let source = self.root.join(path(&layer.normalized()?)?);
            let size = source.metadata()?.len();
            (source, header.offset_data, size)
        };
        if header
            .offset_data
            .checked_add(stored_size)
            .is_none_or(|end| end > bound)
        {
            return Err(io::Error::other("member exceeds layer bounds"));
        }
        let mut reader = archive::open(&source)?;
        if archive::drain(&mut (&mut reader).take(offset))? != offset {
            return Err(io::Error::other("truncated archive before member"));
        }
        let mut destination = File::create(output)?;
        if let Some(map) = &header.sparse {
            for &(start, size) in map {
                if size == 0 {
                    continue;
                }
                destination.seek(SeekFrom::Start(start))?;
                if io::copy(&mut (&mut reader).take(size), &mut destination)? != size {
                    return Err(io::Error::other("truncated sparse member"));
                }
            }
            destination.set_len(header.size)?;
        } else if io::copy(&mut (&mut reader).take(header.size), &mut destination)? != header.size {
            return Err(io::Error::other("truncated archive member"));
        }
        destination.set_permissions(fs::Permissions::from_mode(header.mode & 0o777))?;
        Ok(true)
    }

    pub fn run(
        args: &[std::ffi::OsString],
        output: &mut impl Write,
    ) -> Result<bool, command::Error> {
        use command::Operation;
        let operation = Operation::parse(args)?;
        if operation == Operation::Help {
            output.write_all(b"Inspect emitted image artifacts.\n\nOperations: prepare, paths, digests, extract, exists, link, find.\n")?;
            return Ok(true);
        }
        if operation == Operation::Prepare {
            Self::prepare(Path::new(&args[1]), Path::new(&args[2]))?;
            return Ok(true);
        }
        let image = Self::load(Path::new(&args[1]), Path::new(&args[2]))?;
        if operation == Operation::Digests {
            for layer in &image.layers {
                let digest = image
                    .digests
                    .get(layer)
                    .ok_or_else(|| io::Error::other("missing layer digest"))?
                    .text()?
                    .bytes()?;
                output.write_all(&digest)?;
                output.write_all(b"\n")?;
            }
            return Ok(true);
        }
        if operation == Operation::Paths {
            let pattern = regex::bytes::Regex::new(r"(?-u)(?:^|/)nix/store/[a-z0-9]{32}-[^/]+")
                .map_err(io::Error::other)?;
            let mut paths = BTreeSet::new();
            for name in image.names()? {
                let bytes = name.bytes()?;
                if let Some(found) = pattern.find(&bytes) {
                    let matched = found
                        .as_bytes()
                        .strip_prefix(b"/")
                        .unwrap_or(found.as_bytes());
                    let mut value = vec![b'/'];
                    value.extend_from_slice(matched);
                    paths.insert(Text::from_bytes(&value));
                }
            }
            if paths.is_empty() {
                output.write_all(b"\n")?;
            }
            for value in paths {
                output.write_all(&value.bytes()?)?;
                output.write_all(b"\n")?;
            }
            return Ok(true);
        }
        let member = Text::from_bytes(args[3].as_bytes());
        if operation == Operation::Find {
            return Ok(image.find(&member, output)?);
        }
        let Some((_, header)) = image.lookup(&member)? else {
            return Ok(false);
        };
        match operation {
            Operation::Extract => Ok(image.extract(&member, Path::new(&args[4]))?),
            Operation::Link => {
                if header.kind != b'2' {
                    return Err(io::Error::other("not a symlink").into());
                }
                output.write_all(&header.link.bytes()?)?;
                output.write_all(b"\n")?;
                Ok(true)
            }
            Operation::Exists => Ok(true),
            Operation::Prepare
            | Operation::Paths
            | Operation::Digests
            | Operation::Find
            | Operation::Help => Err(command::Error::Usage),
        }
    }

    fn find(&self, pattern: &Text, output: &mut impl Write) -> io::Result<bool> {
        let pattern = pattern.unicode()?;
        pattern::validate(&pattern)?;
        let expression = regex::bytes::Regex::new(&format!("(?-u)\\A(?:{pattern})\\z"))
            .map_err(io::Error::other)?;
        let mut found = false;
        for name in self.names()? {
            if expression.is_match(&name.bytes()?) {
                output.write_all(&name.bytes()?)?;
                output.write_all(b"\n")?;
                found = true;
            }
        }
        if !found {
            output.write_all(b"\n")?;
        }
        Ok(found)
    }
}
