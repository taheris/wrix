use std::{
    collections::BTreeSet,
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Output, Stdio},
};

use displaydoc::Display;
use fs2::FileExt;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    NixDescriptor,
    DockerArchive,
}

impl SourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NixDescriptor => "nix-descriptor",
            Self::DockerArchive => "docker-archive",
        }
    }
}

/// A source path paired with its wire format; availability is checked when read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source {
    path: PathBuf,
    kind: SourceKind,
}

impl Source {
    pub fn parse(path: impl Into<PathBuf>, kind: SourceKind) -> Result<Self, SourceParseError> {
        let path = path.into();
        if path.as_os_str().is_empty() || path.as_os_str().as_encoded_bytes().contains(&0) {
            return Err(SourceParseError);
        }
        Ok(Self { path, kind })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub const fn kind(&self) -> SourceKind {
        self.kind
    }
}

#[derive(Clone, Debug, Display, Error)]
/// image source must be a nonempty path without NUL bytes
pub struct SourceParseError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Runtime {
    Podman,
    Container,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    pub fn parse(value: &str) -> Result<Self, DigestParseError> {
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(DigestParseError {
                value: value.to_owned(),
            });
        };
        if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(DigestParseError {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Display, Error)]
/// invalid sha256 content digest: {value}
pub struct DigestParseError {
    value: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ImageId(String);

impl ImageId {
    pub fn parse(value: &str) -> Result<Self, ImageIdParseError> {
        let Some(value) = normalized_value(value) else {
            return Err(ImageIdParseError {
                value: value.to_owned(),
            });
        };
        if !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_' | b'.')
            })
        {
            return Err(ImageIdParseError { value });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ImageId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Display, Error)]
/// invalid image ID: {value}
pub struct ImageIdParseError {
    value: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ImageRef(String);

impl ImageRef {
    pub fn parse(value: &str) -> Result<Self, ImageRefParseError> {
        let Some(value) = normalized_value(value) else {
            return Err(ImageRefParseError {
                value: value.to_owned(),
            });
        };
        if !valid_image_reference(&value) {
            return Err(ImageRefParseError { value });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ImageRef {
    fn latest_tag(&self) -> Option<Self> {
        let name = self.0.split('@').next()?;
        let colon = name.rfind(':')?;
        if name.rfind('/').is_some_and(|slash| colon < slash) {
            return None;
        }
        // Parsing already proved the repository; replacing its tag preserves that proof.
        Some(Self(format!("{}:latest", &name[..colon])))
    }
}

fn valid_image_reference(value: &str) -> bool {
    if value.starts_with('-') || value.contains("://") {
        return false;
    }
    let mut digest_parts = value.split('@');
    let Some(name) = digest_parts.next() else {
        return false;
    };
    let digest = digest_parts.next();
    if digest_parts.next().is_some() || digest.is_some_and(|digest| Digest::parse(digest).is_err())
    {
        return false;
    }
    let last_slash = name.rfind('/');
    let last_colon = name.rfind(':');
    let (repository, tag) = match last_colon {
        Some(index) if last_slash.is_none_or(|slash| index > slash) => {
            (&name[..index], Some(&name[index + 1..]))
        }
        _ => (name, None),
    };
    valid_repository(repository) && tag.is_none_or(valid_image_tag)
}

fn valid_repository(value: &str) -> bool {
    let mut components = value.split('/');
    let Some(first) = components.next() else {
        return false;
    };
    valid_registry_or_component(first) && components.all(valid_repository_component)
}

fn valid_registry_or_component(value: &str) -> bool {
    if let Some((host, port)) = value.rsplit_once(':') {
        return valid_repository_component(host)
            && !port.is_empty()
            && port.bytes().all(|byte| byte.is_ascii_digit());
    }
    valid_repository_component(value)
}

fn valid_repository_component(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn valid_image_tag(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

impl<'de> Deserialize<'de> for ImageRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Display, Error)]
/// invalid image reference: {value}
pub struct ImageRefParseError {
    value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    Reference(ImageRef),
    Id(ImageId),
    Digest(Digest),
}

impl Target {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Reference(value) => value.as_str(),
            Self::Id(value) => value.as_str(),
            Self::Digest(value) => value.as_str(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageRow {
    pub target: Target,
    pub id: Option<ImageId>,
}

#[derive(Clone, Debug)]
enum InstallSource<'a> {
    PodmanDescriptor(&'a Source),
    PodmanArchive(&'a Source),
    ContainerArchive(&'a Source),
}

#[derive(Clone, Debug)]
pub struct InstallRequest<'a> {
    image_ref: &'a ImageRef,
    source: InstallSource<'a>,
    digest: Option<&'a Digest>,
}

impl<'a> InstallRequest<'a> {
    pub const fn new(
        runtime: Runtime,
        image_ref: &'a ImageRef,
        source: &'a Source,
        digest: Option<&'a Digest>,
    ) -> Result<Self, Error> {
        let source = match (runtime, source.kind()) {
            (Runtime::Podman, SourceKind::NixDescriptor) => InstallSource::PodmanDescriptor(source),
            (Runtime::Podman, SourceKind::DockerArchive) => InstallSource::PodmanArchive(source),
            (Runtime::Container, SourceKind::DockerArchive) => {
                InstallSource::ContainerArchive(source)
            }
            (Runtime::Container, SourceKind::NixDescriptor) => {
                return Err(Error::UnsupportedSourceKind {
                    kind: source.kind().as_str(),
                });
            }
        };
        Ok(Self {
            image_ref,
            source,
            digest,
        })
    }

    const fn runtime(&self) -> Runtime {
        match self.source {
            InstallSource::PodmanDescriptor(_) | InstallSource::PodmanArchive(_) => Runtime::Podman,
            InstallSource::ContainerArchive(_) => Runtime::Container,
        }
    }

    const fn image_source(&self) -> &Source {
        match self.source {
            InstallSource::PodmanDescriptor(source)
            | InstallSource::PodmanArchive(source)
            | InstallSource::ContainerArchive(source) => source,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RetentionRequest<'a> {
    pub runtime: Runtime,
    pub image_ref: &'a ImageRef,
    pub source: Option<&'a Source>,
    pub digest: Option<&'a Digest>,
    pub mru_path: &'a Path,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OciSource {
    pub digest: Digest,
    pub layout: String,
    pub reference: String,
    pub layers: Vec<Layer>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Layer {
    pub digest: Digest,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Record {
    #[serde(
        default,
        rename = "ref",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_image_ref"
    )]
    pub ref_name: Option<ImageRef>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_digest"
    )]
    pub digest: Option<Digest>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_image_id"
    )]
    pub id: Option<ImageId>,
}

#[expect(
    clippy::doc_markdown,
    reason = "displaydoc comments are user-facing CLI errors and must not add Markdown backticks"
)]
#[derive(Debug, Display, Error)]
pub enum Error {
    /// command failed: {program}: {stderr}
    ProcessFailed { program: String, stderr: String },
    /// nix-descriptor image source is missing a sha256 digest: {path}
    MissingDescriptorDigest { path: String },
    /// nix-descriptor image source is missing oci_layout: {path}
    MissingDescriptorLayout { path: String },
    /// docker-archive image source is missing a sha256 digest: {path}
    MissingDockerArchiveDigest { path: String },
    /// unsupported image source_kind: {kind}
    UnsupportedSourceKind { kind: &'static str },
    /// {error}; failed to remove temporary image directory {path}: {source}
    TemporaryImageCleanup {
        error: Box<Error>,
        path: String,
        source: io::Error,
    },
    /// invalid image descriptor JSON: {source}
    DescriptorJson { source: serde_json::Error },
    /// invalid image MRU JSON: {source}
    MruJson { source: serde_json::Error },
    /// {source}
    Digest { source: DigestParseError },
    /// {source}
    ImageId { source: ImageIdParseError },
    /// image ID is unavailable for store target: {target}
    MissingImageId { target: String },
    /// {source}
    ImageRef { source: ImageRefParseError },
    /// {source}
    Io { source: io::Error },
}

impl From<DigestParseError> for Error {
    fn from(source: DigestParseError) -> Self {
        Self::Digest { source }
    }
}

impl From<ImageIdParseError> for Error {
    fn from(source: ImageIdParseError) -> Self {
        Self::ImageId { source }
    }
}

impl From<ImageRefParseError> for Error {
    fn from(source: ImageRefParseError) -> Self {
        Self::ImageRef { source }
    }
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

pub trait Store {
    fn image_for_digest(
        &mut self,
        runtime: Runtime,
        digest: &Digest,
    ) -> Result<Option<Target>, Error>;
    fn tag(&mut self, runtime: Runtime, source: &Target, target: &ImageRef) -> Result<(), Error>;
    fn copy_oci_layout(&mut self, source: &OciSource, destination: &ImageRef) -> Result<(), Error>;
    fn copy_docker_archive(&mut self, archive: &Path, destination: &ImageRef) -> Result<(), Error>;
    fn load_docker_archive(&mut self, archive: &Path) -> Result<Option<ImageRef>, Error>;
    fn docker_archive_config_digest(&mut self, archive: &Path) -> Result<Option<Digest>, Error>;
    fn image_rows(&mut self, runtime: Runtime) -> Result<Vec<ImageRow>, Error>;
    fn image_id(&mut self, runtime: Runtime, target: &Target) -> Result<Option<ImageId>, Error>;
    fn image_digest(&mut self, runtime: Runtime, target: &Target) -> Result<Option<Digest>, Error>;
    fn image_managed(&mut self, runtime: Runtime, target: &Target) -> Result<bool, Error>;
    fn image_in_use(&mut self, runtime: Runtime, target: &Target) -> Result<bool, Error>;
    fn delete_image(&mut self, runtime: Runtime, target: &Target) -> Result<(), Error>;
}

#[derive(Default)]
pub struct CommandStore;

pub fn install(store: &mut impl Store, request: &InstallRequest<'_>) -> Result<(), Error> {
    let desired = desired_digest(store, request.image_source(), request.digest)?;
    let runtime = request.runtime();
    let selected = Target::Reference(request.image_ref.clone());
    if let Some(source) = store.image_for_digest(runtime, &desired)? {
        if source != selected {
            store.tag(runtime, &source, request.image_ref)?;
        }
        return Ok(());
    }

    match request.source {
        InstallSource::PodmanDescriptor(source) => {
            let descriptor = read_descriptor(source.path())?;
            let source = descriptor.oci_source(source.path())?;
            store.copy_oci_layout(&source, request.image_ref)?;
        }
        InstallSource::PodmanArchive(source) => {
            store.copy_docker_archive(source.path(), request.image_ref)?;
        }
        InstallSource::ContainerArchive(source) => {
            if let Some(untagged) = store.load_docker_archive(source.path())? {
                let untagged = Target::Reference(untagged);
                store.tag(runtime, &untagged, request.image_ref)?;
                store.delete_image(runtime, &untagged)?;
            }
        }
    }

    if runtime == Runtime::Podman
        && let Some(latest) = request.image_ref.latest_tag()
    {
        store.tag(runtime, &selected, &latest)?;
    }
    Ok(())
}

pub fn remember_and_prune(
    store: &mut impl Store,
    request: &RetentionRequest<'_>,
) -> Result<(), Error> {
    let digest = request
        .source
        .map(|source| desired_digest(store, source, request.digest))
        .transpose()?
        .or_else(|| request.digest.cloned());
    with_mru_lock(request.mru_path, || {
        remember(
            store,
            request.runtime,
            request.mru_path,
            request.image_ref,
            digest.as_ref(),
        )?;
        prune(
            store,
            request.runtime,
            request.mru_path,
            request.image_ref,
            digest.as_ref(),
        )
    })
}

pub fn default_mru_path() -> PathBuf {
    if let Some(path) = env::var_os("WRIX_IMAGE_KEEP_FILE") {
        return PathBuf::from(path);
    }
    env::var_os("XDG_CACHE_HOME")
        .map_or_else(|| home_dir().join(".cache"), PathBuf::from)
        .join("wrix/image-mru.json")
}

fn desired_digest(
    store: &mut impl Store,
    source: &Source,
    digest: Option<&Digest>,
) -> Result<Digest, Error> {
    if let Some(digest) = digest {
        return Ok(digest.clone());
    }
    match source.kind() {
        SourceKind::NixDescriptor => {
            read_descriptor(source.path())?
                .digest
                .ok_or_else(|| Error::MissingDescriptorDigest {
                    path: source.path().display().to_string(),
                })
        }
        SourceKind::DockerArchive => store
            .docker_archive_config_digest(source.path())?
            .ok_or_else(|| Error::MissingDockerArchiveDigest {
                path: source.path().display().to_string(),
            }),
    }
}

fn remember(
    store: &mut impl Store,
    runtime: Runtime,
    path: &Path,
    image_ref: &ImageRef,
    digest: Option<&Digest>,
) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let id = store.image_id(runtime, &Target::Reference(image_ref.clone()))?;
    let mut records = read_records(path)?;
    records.insert(
        0,
        Record {
            ref_name: Some(image_ref.clone()),
            digest: digest.cloned(),
            id,
        },
    );
    let mut seen = BTreeSet::new();
    let records = records
        .into_iter()
        .filter(|record| seen.insert(record.clone()))
        .take(8)
        .collect::<Vec<_>>();
    let json =
        serde_json::to_string_pretty(&records).map_err(|source| Error::MruJson { source })?;
    let temporary = path.with_extension(format!("mru-{}.tmp", std::process::id()));
    fs::write(&temporary, format!("{json}\n"))?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn prune(
    store: &mut impl Store,
    runtime: Runtime,
    mru_path: &Path,
    image_ref: &ImageRef,
    digest: Option<&Digest>,
) -> Result<(), Error> {
    let mut keep_refs = BTreeSet::new();
    let mut keep_ids = BTreeSet::new();
    let mut keep_digests = BTreeSet::new();
    keep_refs.insert(image_ref.clone());
    let target = Target::Reference(image_ref.clone());
    if let Some(digest) = digest {
        keep_digests.insert(digest.clone());
    }
    if let Some(id) = store.image_id(runtime, &target)? {
        keep_ids.insert(id);
    }
    if let Some(actual_digest) = store.image_digest(runtime, &target)? {
        keep_digests.insert(actual_digest);
    }
    for record in read_records(mru_path)? {
        if let Some(ref_name) = record.ref_name {
            keep_refs.insert(ref_name);
        }
        if let Some(id) = record.id {
            keep_ids.insert(id);
        }
        if let Some(digest) = record.digest {
            keep_digests.insert(digest);
        }
    }
    for image in list_images(store, runtime)? {
        if !image.managed && !image.legacy {
            continue;
        }
        if image
            .ref_name
            .as_ref()
            .is_some_and(|ref_name| keep_refs.contains(ref_name))
            || keep_ids.contains(&image.id)
            || image
                .digest
                .as_ref()
                .is_some_and(|digest| keep_digests.contains(digest))
        {
            continue;
        }
        if store.image_in_use(runtime, &image.target)? {
            continue;
        }
        if let Err(error) = store.delete_image(runtime, &image.target) {
            tracing::warn!(
                ?runtime,
                image_target = image.target.as_str(),
                error = %error,
                "could not prune stale image"
            );
        }
    }
    Ok(())
}

fn with_mru_lock<T>(path: &Path, operation: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    FileExt::lock_exclusive(&lock)?;
    operation()
}

#[derive(Debug, Deserialize)]
struct Descriptor {
    #[serde(default)]
    digest: Option<Digest>,
    #[serde(default)]
    oci_layout: String,
    #[serde(default)]
    oci_ref: Option<String>,
    #[serde(default)]
    layers: Vec<Layer>,
}

impl Descriptor {
    fn oci_source(self, path: &Path) -> Result<OciSource, Error> {
        if self.oci_layout.is_empty() {
            return Err(Error::MissingDescriptorLayout {
                path: path.display().to_string(),
            });
        }
        let digest = self.digest.ok_or_else(|| Error::MissingDescriptorDigest {
            path: path.display().to_string(),
        })?;
        Ok(OciSource {
            digest,
            layout: self.oci_layout,
            reference: self.oci_ref.unwrap_or_else(|| String::from("latest")),
            layers: self.layers,
        })
    }
}

struct ListedImage {
    ref_name: Option<ImageRef>,
    target: Target,
    id: ImageId,
    digest: Option<Digest>,
    managed: bool,
    legacy: bool,
}

fn read_descriptor(path: &Path) -> Result<Descriptor, Error> {
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).map_err(|source| Error::DescriptorJson { source })
}

fn read_records(path: &Path) -> Result<Vec<Record>, Error> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    match serde_json::from_str::<Vec<Record>>(&content) {
        Ok(records) => Ok(records),
        Err(source) => {
            tracing::warn!(
                mru_path = %path.display(),
                error = %source,
                "resetting invalid image MRU"
            );
            Ok(Vec::new())
        }
    }
}

fn list_images(store: &mut impl Store, runtime: Runtime) -> Result<Vec<ListedImage>, Error> {
    let mut images = Vec::new();
    for ImageRow {
        target,
        id: listed_id,
    } in store.image_rows(runtime)?
    {
        let ref_name = match &target {
            Target::Reference(value) => Some(value.clone()),
            _ => None,
        };
        let id = store
            .image_id(runtime, &target)?
            .or(listed_id)
            .ok_or_else(|| Error::MissingImageId {
                target: target.as_str().to_owned(),
            })?;
        let digest = store.image_digest(runtime, &target)?;
        let managed = store.image_managed(runtime, &target)?;
        let legacy = runtime == Runtime::Podman
            && ref_name
                .as_ref()
                .is_some_and(|name| name.as_str().starts_with("localhost/wrix-"));
        images.push(ListedImage {
            ref_name,
            target,
            id,
            digest,
            managed,
            legacy,
        });
    }
    Ok(images)
}

fn image_row_from_line(runtime: Runtime, line: &str) -> Result<Option<ImageRow>, Error> {
    let mut fields = line.split_whitespace();
    let Some(repo) = fields.next() else {
        return Ok(None);
    };
    if runtime == Runtime::Container && repo.eq_ignore_ascii_case("repository") {
        return Ok(None);
    }
    let tag = fields.next().unwrap_or("<none>");
    let id = fields
        .next()
        .and_then(normalized_value)
        .map(|id| ImageId::parse(&id))
        .transpose()?;
    let reference =
        if runtime == Runtime::Container && tag == "<none>" && repo.starts_with("untagged@sha256:")
        {
            Some(ImageRef::parse(repo)?)
        } else if repo != "<none>" && tag != "<none>" {
            Some(ImageRef::parse(&format!("{repo}:{tag}"))?)
        } else {
            None
        };
    Ok(reference
        .map(Target::Reference)
        .or_else(|| id.clone().map(Target::Id))
        .map(|target| ImageRow { target, id }))
}

fn deserialize_optional_image_ref<'de, D>(deserializer: D) -> Result<Option<ImageRef>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .and_then(|value| normalized_value(&value))
        .map(|value| ImageRef::parse(&value).map_err(de::Error::custom))
        .transpose()
}

fn deserialize_optional_digest<'de, D>(deserializer: D) -> Result<Option<Digest>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .and_then(|value| normalized_value(&value))
        .map(|value| Digest::parse(&value).map_err(de::Error::custom))
        .transpose()
}

fn deserialize_optional_image_id<'de, D>(deserializer: D) -> Result<Option<ImageId>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .and_then(|value| normalized_value(&value))
        .map(|value| ImageId::parse(&value).map_err(de::Error::custom))
        .transpose()
}

fn normalized_value(value: &str) -> Option<String> {
    match value.trim() {
        "" | "<none>" | "<no value>" | "null" => None,
        value => Some(value.to_owned()),
    }
}

impl Store for CommandStore {
    fn image_for_digest(
        &mut self,
        runtime: Runtime,
        digest: &Digest,
    ) -> Result<Option<Target>, Error> {
        match runtime {
            Runtime::Podman => {
                let output = run_output(
                    "podman",
                    &["image", "inspect", "--format", "{{.Id}}", digest.as_str()],
                )?;
                Ok(output
                    .status
                    .success()
                    .then(|| Target::Digest(digest.clone())))
            }
            Runtime::Container => darwin_image_for_digest(digest),
        }
    }

    fn tag(&mut self, runtime: Runtime, source: &Target, target: &ImageRef) -> Result<(), Error> {
        let source = source.as_str();
        let target = target.as_str();
        let (program, output) = match runtime {
            Runtime::Podman => ("podman", run_output("podman", &["tag", source, target])?),
            Runtime::Container => (
                "container",
                run_output("container", &["image", "tag", source, target])?,
            ),
        };
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        tracing::error!(
            ?runtime,
            image_source = %source,
            image_target = %target,
            error = %stderr,
            "failed to tag image"
        );
        Err(Error::ProcessFailed {
            program: program.to_owned(),
            stderr,
        })
    }

    fn copy_oci_layout(&mut self, source: &OciSource, destination: &ImageRef) -> Result<(), Error> {
        let destination = linux_store_ref(destination);
        run_required(
            "skopeo",
            &[
                "--insecure-policy",
                "copy",
                "--quiet",
                &format!("oci:{}:{}", source.layout, source.reference),
                &destination,
            ],
        )
    }

    fn copy_docker_archive(&mut self, archive: &Path, destination: &ImageRef) -> Result<(), Error> {
        let destination = linux_store_ref(destination);
        run_required(
            "skopeo",
            &[
                "--insecure-policy",
                "copy",
                "--quiet",
                &format!("docker-archive:{}", archive.display()),
                &destination,
            ],
        )
    }

    fn load_docker_archive(&mut self, archive: &Path) -> Result<Option<ImageRef>, Error> {
        let temp_dir = create_temp_dir("wrix-image")?;
        let result = load_container_archive(archive, &temp_dir);
        match (result, fs::remove_dir_all(&temp_dir)) {
            (Ok(loaded), Ok(())) => Ok(loaded),
            (Ok(_loaded), Err(source)) => Err(source.into()),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(source)) => Err(Error::TemporaryImageCleanup {
                error: Box::new(error),
                path: temp_dir.display().to_string(),
                source,
            }),
        }
    }

    fn docker_archive_config_digest(&mut self, archive: &Path) -> Result<Option<Digest>, Error> {
        let output = run_required_output(
            "skopeo",
            &[
                "inspect",
                "--raw",
                &format!("docker-archive:{}", archive.display()),
            ],
        )?;
        let value = serde_json::from_slice::<Value>(&output.stdout)
            .map_err(|source| Error::DescriptorJson { source })?;
        value
            .pointer("/config/digest")
            .and_then(Value::as_str)
            .and_then(normalized_value)
            .map(|value| Digest::parse(&value).map_err(Error::from))
            .transpose()
    }

    fn image_rows(&mut self, runtime: Runtime) -> Result<Vec<ImageRow>, Error> {
        match runtime {
            Runtime::Podman => {
                let output = run_required_output(
                    "podman",
                    &["images", "--format", "{{.Repository}} {{.Tag}} {{.ID}}"],
                )?;
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(|line| image_row_from_line(runtime, line).transpose())
                    .collect()
            }
            Runtime::Container => {
                let output =
                    run_required_output("container", &["image", "list", "--format", "json"])?;
                container_image_rows(&output.stdout)
            }
        }
    }

    fn image_id(&mut self, runtime: Runtime, target: &Target) -> Result<Option<ImageId>, Error> {
        inspect_value(runtime, target, InspectField::Id)?
            .map(|value| ImageId::parse(&value).map_err(Error::from))
            .transpose()
    }

    fn image_digest(&mut self, runtime: Runtime, target: &Target) -> Result<Option<Digest>, Error> {
        inspect_value(runtime, target, InspectField::Digest)?
            .map(|value| Digest::parse(&value).map_err(Error::from))
            .transpose()
    }

    fn image_managed(&mut self, runtime: Runtime, target: &Target) -> Result<bool, Error> {
        Ok(inspect_value(runtime, target, InspectField::Managed)?.as_deref() == Some("true"))
    }

    fn image_in_use(&mut self, runtime: Runtime, target: &Target) -> Result<bool, Error> {
        match runtime {
            Runtime::Podman => {
                let filter = format!("ancestor={}", target.as_str());
                let output = run_required_output(
                    "podman",
                    &["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"],
                )?;
                Ok(!trim_stdout(&output.stdout).is_empty())
            }
            Runtime::Container => {
                let id = self.image_id(runtime, target)?;
                let output =
                    run_required_output("container", &["list", "--all", "--format", "json"])?;
                container_image_in_use(&output.stdout, target, id.as_ref())
            }
        }
    }

    fn delete_image(&mut self, runtime: Runtime, target: &Target) -> Result<(), Error> {
        let target = target.as_str();
        let (program, output) = match runtime {
            Runtime::Podman => ("podman", run_output("podman", &["rmi", target])?),
            Runtime::Container => (
                "container",
                run_output("container", &["image", "delete", target])?,
            ),
        };
        if output.status.success() {
            return Ok(());
        }
        Err(Error::ProcessFailed {
            program: program.to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn linux_store_ref(image_ref: &ImageRef) -> String {
    // Store discovery is optional: containers-storage resolves its configured default
    // when Podman cannot supply explicit graph/run roots.
    let image_ref = image_ref.as_str();
    let mut store_ref = format!("containers-storage:{image_ref}");
    if let Ok(output) = run_output(
        "podman",
        &[
            "info",
            "--format",
            "{{.Store.GraphDriverName}}@{{.Store.GraphRoot}}+{{.Store.RunRoot}}",
        ],
    ) && output.status.success()
    {
        let spec = trim_stdout(&output.stdout);
        if spec.contains('@') && spec.contains('+') {
            store_ref = format!("containers-storage:[{spec}]{image_ref}");
        }
    }
    store_ref
}

#[derive(Clone, Copy)]
enum InspectField {
    Id,
    Digest,
    Managed,
}

fn inspect_value(
    runtime: Runtime,
    target: &Target,
    field: InspectField,
) -> Result<Option<String>, Error> {
    let target = target.as_str();
    let output = match runtime {
        Runtime::Podman => {
            let format = match field {
                InspectField::Id => "{{.Id}}",
                InspectField::Digest => "{{.Digest}}",
                InspectField::Managed => "{{ index .Config.Labels \"wrix.managed\" }}",
            };
            run_output("podman", &["image", "inspect", "--format", format, target])?
        }
        Runtime::Container => run_output("container", &["image", "inspect", target])?,
    };
    if !output.status.success() {
        return Ok(None);
    }
    match runtime {
        Runtime::Podman => Ok(normalized_value(&trim_stdout(&output.stdout))),
        Runtime::Container => inspect_container_value(&output.stdout, field),
    }
}

fn inspect_container_value(stdout: &[u8], field: InspectField) -> Result<Option<String>, Error> {
    let value = serde_json::from_slice::<Value>(stdout)
        .map_err(|source| Error::DescriptorJson { source })?;
    let pointer = match field {
        InspectField::Id => "/0/id",
        InspectField::Digest => "/0/digest",
        InspectField::Managed => "/0/labels/wrix.managed",
    };
    let value = value.pointer(pointer).or_else(|| match field {
        InspectField::Managed => value
            .pointer("/0/Labels/wrix.managed")
            .or_else(|| container_variant_label(&value, "wrix.managed")),
        _ => None,
    });
    Ok(value.and_then(Value::as_str).and_then(normalized_value))
}

fn container_variant_label<'a>(image: &'a Value, label: &str) -> Option<&'a Value> {
    image
        .pointer("/0/variants")?
        .as_array()?
        .iter()
        .find_map(|variant| {
            variant
                .pointer("/config/config/Labels")?
                .as_object()?
                .get(label)
        })
}

fn container_image_rows(stdout: &[u8]) -> Result<Vec<ImageRow>, Error> {
    let value = serde_json::from_slice::<Value>(stdout)
        .map_err(|source| Error::DescriptorJson { source })?;
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|image| container_image_row(image).transpose())
        .collect()
}

fn container_image_row(image: &Value) -> Result<Option<ImageRow>, Error> {
    let Some(name) = image.pointer("/configuration/name").and_then(Value::as_str) else {
        return Ok(None);
    };
    let name = name.strip_prefix("docker.io/library/").unwrap_or(name);
    let Some(id) = image.pointer("/id").and_then(Value::as_str) else {
        return Ok(None);
    };
    let reference = if name.starts_with("untagged@sha256:") || name.contains(':') {
        ImageRef::parse(name)?
    } else {
        ImageRef::parse(&format!("{name}:latest"))?
    };
    Ok(Some(ImageRow {
        target: Target::Reference(reference),
        id: Some(ImageId::parse(id)?),
    }))
}

fn container_image_in_use(
    stdout: &[u8],
    target: &Target,
    id: Option<&ImageId>,
) -> Result<bool, Error> {
    let value = serde_json::from_slice::<Value>(stdout)
        .map_err(|source| Error::DescriptorJson { source })?;
    let target = target.as_str();
    let target = target.strip_prefix("docker.io/library/").unwrap_or(target);
    let id = id.map(|value| value.as_str().trim_start_matches("sha256:"));
    Ok(value.as_array().into_iter().flatten().any(|container| {
        let reference = container
            .pointer("/configuration/image/reference")
            .and_then(Value::as_str)
            .map(|reference| {
                reference
                    .strip_prefix("docker.io/library/")
                    .unwrap_or(reference)
            });
        let descriptor = container
            .pointer("/configuration/image/descriptor/digest")
            .and_then(Value::as_str)
            .map(|digest| digest.trim_start_matches("sha256:"));
        reference == Some(target) || id.is_some_and(|id| descriptor == Some(id))
    }))
}

fn darwin_image_for_digest(digest: &Digest) -> Result<Option<Target>, Error> {
    let output = run_output("container", &["image", "list"])?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines().skip(1) {
        let Some(reference) = container_reference_from_line(line)? else {
            continue;
        };
        let inspect = run_output("container", &["image", "inspect", reference.as_str()])?;
        if !inspect.status.success() {
            continue;
        }
        let value = serde_json::from_slice::<Value>(&inspect.stdout)
            .map_err(|source| Error::DescriptorJson { source })?;
        if container_content_digest(&value)?.as_ref() == Some(digest) {
            return Ok(Some(Target::Reference(reference)));
        }
    }
    Ok(None)
}

fn container_content_digest(value: &Value) -> Result<Option<Digest>, Error> {
    if let Some(actual) = value.pointer("/0/digest").and_then(Value::as_str) {
        let normalized = format!(
            "sha256:{}",
            actual.strip_prefix("sha256:").unwrap_or(actual)
        );
        return Ok(Some(Digest::parse(&normalized)?));
    }
    let Some(id) = value.pointer("/0/id").and_then(Value::as_str) else {
        return Ok(None);
    };
    let id = ImageId::parse(id)?;
    let value = id.as_str();
    if value.starts_with("sha256:") {
        Ok(Some(Digest::parse(value)?))
    } else if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(Some(Digest::parse(&format!("sha256:{value}"))?))
    } else {
        // Opaque runtime IDs are valid, but are not content-digest evidence.
        Ok(None)
    }
}

fn container_reference_from_line(line: &str) -> Result<Option<ImageRef>, Error> {
    let mut fields = line.split_whitespace();
    let Some(repo) = fields.next() else {
        return Ok(None);
    };
    if repo.eq_ignore_ascii_case("repository") {
        return Ok(None);
    }
    let Some(tag) = fields.next() else {
        return Ok(None);
    };
    if tag == "<none>" && repo.starts_with("untagged@sha256:") {
        return Ok(Some(ImageRef::parse(repo)?));
    }
    Ok(Some(ImageRef::parse(&format!("{repo}:{tag}"))?))
}

fn create_temp_dir(prefix: &str) -> Result<PathBuf, Error> {
    for attempt in 0..100 {
        let path = env::temp_dir().join(format!("{prefix}-{}-{attempt}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("could not create a unique {prefix} temporary directory"),
    )
    .into())
}

fn load_container_archive(archive: &Path, temp_dir: &Path) -> Result<Option<ImageRef>, Error> {
    let oci_archive = temp_dir.join("image.oci");
    let source = format!("docker-archive:{}", archive.display());
    let destination = format!("oci-archive:{}", oci_archive.display());
    run_required(
        "skopeo",
        &[
            "--insecure-policy",
            "copy",
            "--quiet",
            &source,
            &destination,
        ],
    )?;
    let output = run_required_output(
        "container",
        &[
            "image",
            "load",
            "--input",
            &oci_archive.display().to_string(),
        ],
    )?;
    loaded_container_ref(&output.stdout, &output.stderr)
}

fn loaded_container_ref(stdout: &[u8], stderr: &[u8]) -> Result<Option<ImageRef>, Error> {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    for token in text.split_whitespace() {
        let token = token.trim_matches(|ch| matches!(ch, '"' | '\'' | ',' | ';'));
        if token.starts_with("untagged@sha256:") {
            return Ok(Some(ImageRef::parse(token)?));
        }
    }
    Ok(None)
}

fn run_required(program: &str, args: &[&str]) -> Result<(), Error> {
    let output = run_output(program, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::ProcessFailed {
            program: program.to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn run_required_output(program: &str, args: &[&str]) -> Result<Output, Error> {
    let output = run_output(program, args)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(Error::ProcessFailed {
            program: program.to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn run_output(program: &str, args: &[&str]) -> Result<Output, Error> {
    ProcessCommand::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::from)
}

fn trim_stdout(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout).trim().to_owned()
}

fn home_dir() -> PathBuf {
    env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use super::{
        ImageId, ImageRef, InspectField, Runtime, Target, container_image_in_use,
        container_image_rows, image_row_from_line, inspect_container_value, loaded_container_ref,
    };

    #[test]
    fn digest_rejects_non_sha256_values() {
        assert!(super::Digest::parse("sha512:abc").is_err());
    }

    #[test]
    fn image_id_rejects_sentinel_values() {
        assert!(super::ImageId::parse("<none>").is_err());
    }

    #[test]
    fn image_id_rejects_whitespace() {
        assert!(super::ImageId::parse("image id").is_err());
    }

    #[test]
    fn image_ref_accepts_registry_port_tag_and_digest() {
        assert!(super::ImageRef::parse("localhost:5000/wrix/image:test-1").is_ok());
        assert!(super::ImageRef::parse(&format!("wrix/image@sha256:{}", "a".repeat(64))).is_ok());
    }

    #[test]
    fn image_ref_rejects_option_like_and_malformed_values() {
        for value in [
            "--privileged",
            "localhost/wrix image:latest",
            "docker://wrix:test",
            "wrix//image:test",
            "wrix/image:",
            "wrix/image@sha256:short",
        ] {
            assert!(super::ImageRef::parse(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn apple_content_digest_accepts_prefixed_bare_and_id_fallback_variants() {
        let hex = "a".repeat(64);
        let expected = super::Digest::parse(&format!("sha256:{hex}")).unwrap();
        for value in [
            json!([{"digest":expected}]),
            json!([{"digest":hex}]),
            json!([{"id":expected}]),
            json!([{"digest":null,"id":hex}]),
        ] {
            assert_eq!(
                super::container_content_digest(&value).unwrap().as_ref(),
                Some(&expected)
            );
        }
        assert!(
            super::container_content_digest(&json!([{"id":"opaque-image-id"}]))
                .unwrap()
                .is_none()
        );
        assert!(super::container_content_digest(&json!([{"digest":"sha256:short"}])).is_err());
    }

    #[test]
    fn podman_rows_parse_typed_references_ids_and_absent_fields() {
        let row = image_row_from_line(Runtime::Podman, "localhost/wrix-test old image-id")
            .unwrap()
            .unwrap();
        assert_eq!(
            row.target,
            Target::Reference(ImageRef::parse("localhost/wrix-test:old").unwrap())
        );
        assert_eq!(row.id, Some(ImageId::parse("image-id").unwrap()));
        let named_repository = image_row_from_line(Runtime::Podman, "repository latest image-id")
            .unwrap()
            .unwrap();
        assert_eq!(
            named_repository.target,
            Target::Reference(ImageRef::parse("repository:latest").unwrap())
        );
        let dangling = image_row_from_line(Runtime::Podman, "<none> <none> dangling-id")
            .unwrap()
            .unwrap();
        assert_eq!(
            dangling.target,
            Target::Id(ImageId::parse("dangling-id").unwrap())
        );
        assert!(
            image_row_from_line(Runtime::Podman, "<none> <none> <none>")
                .unwrap()
                .is_none()
        );
        for value in ["--all", "<none>", "id\0suffix", "id with space", "id/path"] {
            assert!(ImageId::parse(value).is_err(), "accepted {value:?}");
        }
        assert!(image_row_from_line(Runtime::Podman, "<none> <none> --all").is_err());
        assert!(image_row_from_line(Runtime::Podman, "--all latest image-id").is_err());
    }

    #[test]
    fn installation_constructor_rejects_contradictory_sources() {
        use super::{InstallRequest, Source, SourceKind};
        assert!(Source::parse("", SourceKind::NixDescriptor).is_err());
        assert!(Source::parse("/image\0", SourceKind::DockerArchive).is_err());
        let reference = ImageRef::parse("wrix-test:latest").unwrap();
        let descriptor = Source::parse("/not-yet-realized", SourceKind::NixDescriptor).unwrap();
        assert!(InstallRequest::new(Runtime::Container, &reference, &descriptor, None).is_err());
        assert!(InstallRequest::new(Runtime::Podman, &reference, &descriptor, None).is_ok());
        let archive = Source::parse("/not-yet-realized", SourceKind::DockerArchive).unwrap();
        for runtime in [Runtime::Podman, Runtime::Container] {
            assert!(InstallRequest::new(runtime, &reference, &archive, None).is_ok());
        }
    }

    #[test]
    fn mru_round_trips_typed_identifiers_and_accepts_legacy_empty_fields() {
        let value = json!({"ref":"localhost/wrix-test:old", "id":"sha256:image-id", "digest": format!("sha256:{}", "c".repeat(64))});
        let record: super::Record = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(record).unwrap(), value);
        let record: super::Record =
            serde_json::from_value(json!({"ref":"", "id":"<none>", "digest":"null"})).unwrap();
        assert!(record.ref_name.is_none() && record.id.is_none() && record.digest.is_none());
    }

    #[test]
    fn latest_tag_preserves_repository_without_corrupting_digest_references() {
        let reference = ImageRef::parse(&format!(
            "localhost:5000/wrix-test:old@sha256:{}",
            "f".repeat(64)
        ))
        .unwrap();
        assert_eq!(
            reference.latest_tag(),
            Some(ImageRef::parse("localhost:5000/wrix-test:latest").unwrap())
        );
        assert!(
            ImageRef::parse("localhost:5000/wrix-test")
                .unwrap()
                .latest_tag()
                .is_none()
        );
    }

    #[test]
    fn apple_load_output_parser_extracts_untagged_ref() {
        let reference = format!("untagged@sha256:{}", "a".repeat(64));
        let output = format!("loading\nLoaded: {reference}, done\n");
        assert_eq!(
            loaded_container_ref(output.as_bytes(), b"").expect("parse load output"),
            Some(ImageRef::parse(&reference).expect("valid reference"))
        );
        assert!(loaded_container_ref(b"Loaded: untagged@sha256:short", b"").is_err());
    }

    #[test]
    fn apple_image_list_parser_preserves_full_untagged_reference() {
        let reference = format!("untagged@sha256:{}", "a".repeat(64));
        let output = serde_json::to_vec(&json!([
            {
                "configuration": {
                    "name": "docker.io/library/wrix-rust:abc123"
                },
                "id": "named-index-digest"
            },
            {
                "configuration": {
                    "name": reference
                },
                "id": "untagged-index-digest"
            }
        ]))
        .expect("serialize image list fixture");

        let rows = container_image_rows(&output).expect("parse image list");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].target,
            Target::Reference(ImageRef::parse("wrix-rust:abc123").expect("valid reference"))
        );
        assert_eq!(
            rows[0].id,
            Some(ImageId::parse("named-index-digest").expect("valid ID"))
        );
        assert_eq!(
            rows[1].target,
            Target::Reference(ImageRef::parse(&reference).expect("valid reference"))
        );
        assert_eq!(
            rows[1].id,
            Some(ImageId::parse("untagged-index-digest").expect("valid ID"))
        );
    }

    #[test]
    fn apple_image_inspect_finds_managed_label_in_variant() {
        let output = serde_json::to_vec(&json!([{
            "variants": [{
                "config": {
                    "config": {
                        "Labels": {
                            "wrix.managed": "true"
                        }
                    }
                }
            }]
        }]))
        .expect("serialize image inspect fixture");

        assert_eq!(
            inspect_container_value(&output, InspectField::Managed).expect("parse image inspect"),
            Some(String::from("true"))
        );
    }

    #[test]
    fn apple_untagged_row_uses_full_reference_as_cleanup_target() {
        let reference = format!("untagged@sha256:{}", "b".repeat(64));
        let row = image_row_from_line(
            Runtime::Container,
            &format!("{reference} <none> untagged-index-digest"),
        )
        .expect("parse listed image identity")
        .expect("image row");
        assert_eq!(
            row.target,
            Target::Reference(ImageRef::parse(&reference).expect("valid reference"))
        );
        assert_eq!(
            row.id,
            Some(ImageId::parse("untagged-index-digest").expect("valid ID"))
        );
    }

    #[test]
    fn apple_container_list_preserves_images_used_by_existing_containers() {
        let output = serde_json::to_vec(&json!([{
            "configuration": {
                "image": {
                    "descriptor": {
                        "digest": "sha256:index-digest"
                    },
                    "reference": "docker.io/library/wrix-service:abc123"
                }
            }
        }]))
        .expect("serialize container list fixture");

        assert!(
            container_image_in_use(
                &output,
                &Target::Reference(
                    ImageRef::parse(&format!("untagged@sha256:{}", "a".repeat(64)))
                        .expect("valid reference")
                ),
                Some(&ImageId::parse("index-digest").expect("valid ID"))
            )
            .expect("parse container list")
        );
        assert!(
            container_image_in_use(
                &output,
                &Target::Reference(
                    ImageRef::parse("wrix-service:abc123").expect("valid reference")
                ),
                None
            )
            .expect("parse container list")
        );
        assert!(
            !container_image_in_use(
                &output,
                &Target::Reference(ImageRef::parse("wrix-service:stale").expect("valid reference")),
                Some(&ImageId::parse("stale-index").expect("valid ID"))
            )
            .expect("parse container list")
        );
    }
}
