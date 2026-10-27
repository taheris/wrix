mod apple;
mod command;

/// Scope native image operations to exact package commands with only their explicit environment.
pub fn with_runtime_commands<T, E: From<Error>>(
    container: &ProcessCommand,
    skopeo: &ProcessCommand,
    callback: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    command::scope(container, skopeo, callback)
}

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Output, Stdio},
};

use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
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
    /// runtime image metadata has an unsupported shape
    InvalidMetadata,
    /// {source}
    Digest { source: DigestParseError },
    /// {source}
    ImageId { source: ImageIdParseError },
    /// image does not implement private Nix v1: {image_ref}; rebuild the sandbox and use its matching ProfileConfig
    PrivateNixUnavailable { image_ref: String },
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

/// Passive runtime-store metadata; not authorization to remove an image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Inspection {
    pub target: Target,
    pub id: ImageId,
    pub digest: Option<Digest>,
    pub managed: bool,
    pub in_use: bool,
}

fn read_descriptor(path: &Path) -> Result<Descriptor, Error> {
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).map_err(|source| Error::DescriptorJson { source })
}

/// Inspect the host user's shared runtime store without preparing services or images.
pub fn inventory(store: &mut impl Store, runtime: Runtime) -> Result<Vec<Inspection>, Error> {
    store
        .image_rows(runtime)?
        .into_iter()
        .map(|row| inspect(store, runtime, &row.target))
        .collect()
}

/// Require current identity, ownership and container-reference evidence.
pub fn inspect(
    store: &mut impl Store,
    runtime: Runtime,
    target: &Target,
) -> Result<Inspection, Error> {
    let id = store
        .image_id(runtime, target)?
        .ok_or_else(|| Error::MissingImageId {
            target: target.as_str().to_owned(),
        })?;
    let identity = Target::Id(id.clone());
    Ok(Inspection {
        target: target.clone(),
        digest: store.image_digest(runtime, &identity)?,
        managed: store.image_managed(runtime, &identity)?,
        in_use: store.image_in_use(runtime, &identity)?,
        id,
    })
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
        let source = runtime_selector(runtime, source)?;
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
                    &[
                        "images",
                        "--no-trunc",
                        "--format",
                        "{{.Repository}} {{.Tag}} {{.ID}}",
                    ],
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
        let managed = inspect_value(runtime, target, InspectField::Managed)?;
        let kind = inspect_value(runtime, target, InspectField::Kind)?;
        Ok(managed_ownership(managed.as_deref(), kind.as_deref()))
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
                let id = match target {
                    Target::Id(id) => id.clone(),
                    _ => self
                        .image_id(runtime, target)?
                        .ok_or_else(|| Error::MissingImageId {
                            target: target.as_str().to_owned(),
                        })?,
                };
                let output =
                    run_required_output("container", &["list", "--all", "--format", "json"])?;
                container_image_in_use(&output.stdout, target, Some(&id))
            }
        }
    }

    fn delete_image(&mut self, runtime: Runtime, target: &Target) -> Result<(), Error> {
        let selector = runtime_selector(runtime, target)?;
        let target = selector.as_str();
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

/// Reject incompatible cached or overridden images before starting a sandbox.
pub fn require_private_nix(runtime: Runtime, image_ref: &ImageRef) -> Result<(), Error> {
    let version = inspect_value(
        runtime,
        &Target::Reference(image_ref.clone()),
        InspectField::PrivateNix,
    )?;
    if version.as_deref() != Some("1") {
        return Err(Error::PrivateNixUnavailable {
            image_ref: image_ref.as_str().to_owned(),
        });
    }
    Ok(())
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
    Kind,
    PrivateNix,
}

fn runtime_selector(runtime: Runtime, target: &Target) -> Result<String, Error> {
    match (runtime, target) {
        (Runtime::Container, Target::Id(id)) => apple::reference(id),
        (Runtime::Container, Target::Digest(digest)) => Ok(darwin_image_for_digest(digest)?
            .ok_or_else(|| Error::MissingImageId {
                target: digest.as_str().to_owned(),
            })?
            .as_str()
            .to_owned()),
        _ => Ok(target.as_str().to_owned()),
    }
}

fn inspect_value(
    runtime: Runtime,
    target: &Target,
    field: InspectField,
) -> Result<Option<String>, Error> {
    let selector = runtime_selector(runtime, target)?;
    let output = match runtime {
        Runtime::Podman => {
            let format = match field {
                InspectField::Id => "{{.Id}}",
                InspectField::Digest => "{{.Digest}}",
                InspectField::Managed => "{{ index .Config.Labels \"wrix.managed\" }}",
                InspectField::Kind => "{{ index .Config.Labels \"wrix.image.kind\" }}",
                InspectField::PrivateNix => "{{ index .Config.Labels \"wrix.nix.private\" }}",
            };
            run_output(
                "podman",
                &["image", "inspect", "--format", format, &selector],
            )?
        }
        Runtime::Container => run_output("container", &["image", "inspect", &selector])?,
    };
    if !output.status.success() {
        return Err(Error::ProcessFailed {
            program: match runtime {
                Runtime::Podman => "podman",
                Runtime::Container => "container",
            }
            .to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    match runtime {
        Runtime::Podman => Ok(normalized_value(&trim_stdout(&output.stdout))),
        Runtime::Container => {
            if let Target::Id(expected) = target {
                apple::require_id(&output.stdout, expected)?;
            }
            inspect_container_value(&output.stdout, field)
        }
    }
}

fn managed_ownership(managed: Option<&str>, kind: Option<&str>) -> bool {
    managed == Some("true") && matches!(kind, Some("profile" | "service" | "builder"))
}

fn inspect_container_value(stdout: &[u8], field: InspectField) -> Result<Option<String>, Error> {
    let value = serde_json::from_slice::<Value>(stdout)
        .map_err(|source| Error::DescriptorJson { source })?;
    match field {
        InspectField::Id => Ok(value
            .pointer("/0/id")
            .and_then(Value::as_str)
            .and_then(normalized_value)),
        InspectField::Digest => {
            Ok(container_content_digest(&value)?.map(|digest| digest.as_str().to_owned()))
        }
        InspectField::Managed => consistent_container_label(&value, "wrix.managed"),
        InspectField::Kind => consistent_container_label(&value, "wrix.image.kind"),
        InspectField::PrivateNix => consistent_container_label(&value, "wrix.nix.private"),
    }
}

fn consistent_container_label(image: &Value, label: &str) -> Result<Option<String>, Error> {
    let images = image.as_array().ok_or(Error::InvalidMetadata)?;
    let [image] = images.as_slice() else {
        return Err(Error::InvalidMetadata);
    };
    let mut labels = Vec::new();
    for name in ["labels", "Labels"] {
        if let Some(map) = image.get(name) {
            labels.push(map.get(label).and_then(Value::as_str));
        }
    }
    if let Some(variants) = image.get("variants") {
        for variant in variants.as_array().ok_or(Error::InvalidMetadata)? {
            labels.push(
                variant
                    .pointer("/config/config/Labels")
                    .and_then(|map| map.get(label))
                    .and_then(Value::as_str),
            );
        }
    }
    let Some(Some(first)) = labels.first() else {
        return Ok(None);
    };
    Ok(labels
        .iter()
        .all(|value| *value == Some(*first))
        .then(|| (*first).to_owned()))
}

fn container_image_rows(stdout: &[u8]) -> Result<Vec<ImageRow>, Error> {
    let value = serde_json::from_slice::<Value>(stdout)
        .map_err(|source| Error::DescriptorJson { source })?;
    value
        .as_array()
        .ok_or(Error::InvalidMetadata)?
        .iter()
        .map(|image| container_image_row(image)?.ok_or(Error::InvalidMetadata))
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
    let id = id.map(content_digest_id).transpose()?.flatten();
    let containers = value.as_array().ok_or(Error::InvalidMetadata)?;
    let mut in_use = false;
    for container in containers {
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
            .and_then(Value::as_str);
        let descriptor = Digest::parse(descriptor.ok_or(Error::InvalidMetadata)?)?;
        if reference == Some(target) {
            in_use = true;
        } else {
            let id = id.as_ref().ok_or(Error::InvalidMetadata)?;
            in_use |= &descriptor == id;
        }
    }
    Ok(in_use)
}

fn darwin_image_for_digest(digest: &Digest) -> Result<Option<Target>, Error> {
    let output = run_required_output("container", &["image", "list", "--format", "json"])?;
    for row in container_image_rows(&output.stdout)? {
        let inspect = run_required_output("container", &["image", "inspect", row.target.as_str()])?;
        let value = serde_json::from_slice::<Value>(&inspect.stdout)
            .map_err(|source| Error::DescriptorJson { source })?;
        if container_content_digest(&value)?.as_ref() == Some(digest) {
            return Ok(Some(row.target));
        }
    }
    Ok(None)
}

fn container_content_digest(value: &Value) -> Result<Option<Digest>, Error> {
    if value.pointer("/0/configuration/descriptor").is_some() {
        return apple::cached(value);
    }
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
    content_digest_id(&ImageId::parse(id)?)
}

fn content_digest_id(id: &ImageId) -> Result<Option<Digest>, Error> {
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
    let loaded = loaded_container_ref(&output.stdout, &output.stderr)?;
    if let Some(reference) = &loaded {
        apple::attest(reference, temp_dir)?;
    } else {
        let output = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidMetadata)?;
        for reference in output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            apple::attest(&ImageRef::parse(reference)?, temp_dir)?;
        }
    }
    Ok(loaded)
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
    command::selected(program)
        .unwrap_or_else(|| ProcessCommand::new(program))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::from)
}

fn trim_stdout(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout).trim().to_owned()
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
    fn apple_image_inspect_finds_private_nix_version_in_all_supported_shapes() {
        for image in [
            json!([{"labels": {"wrix.nix.private": "1"}}]),
            json!([{"Labels": {"wrix.nix.private": "1"}}]),
            json!([{"variants": [{"config": {"config": {"Labels": {"wrix.nix.private": "1"}}}}]}]),
        ] {
            assert_eq!(
                inspect_container_value(
                    &serde_json::to_vec(&image).expect("serialize image"),
                    InspectField::PrivateNix
                )
                .expect("parse image"),
                Some(String::from("1"))
            );
        }
        assert_eq!(
            inspect_container_value(b"[{}]", InspectField::PrivateNix).expect("parse image"),
            None
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
    fn ownership_requires_both_labels_for_every_supported_kind() {
        for kind in ["profile", "service", "builder"] {
            assert!(super::managed_ownership(Some("true"), Some(kind)));
            assert!(!super::managed_ownership(None, Some(kind)));
            assert!(!super::managed_ownership(Some("false"), Some(kind)));
        }
        assert!(!super::managed_ownership(Some("true"), None));
        assert!(!super::managed_ownership(Some("true"), Some("unknown")));
    }

    #[test]
    fn apple_ambiguous_ownership_is_not_management_evidence() {
        let output = serde_json::to_vec(&json!([{
            "labels": {"wrix.managed": "true", "wrix.image.kind": "profile"},
            "variants": [{"config": {"config": {"Labels": {
                "wrix.managed": "false", "wrix.image.kind": "service"
            }}}}]
        }]))
        .unwrap();
        for field in [InspectField::Managed, InspectField::Kind] {
            assert_eq!(inspect_container_value(&output, field).unwrap(), None);
        }
    }

    #[test]
    fn apple_uncertain_container_metadata_is_not_unused_evidence() {
        for value in [
            json!({}),
            json!([{}]),
            json!([{"configuration": {
                "image": {"descriptor": {"digest": ""}}
            }}]),
        ] {
            assert!(
                container_image_in_use(
                    &serde_json::to_vec(&value).unwrap(),
                    &Target::Id(ImageId::parse("selected-id").unwrap()),
                    None
                )
                .is_err()
            );
        }
    }

    #[test]
    fn apple_opaque_ids_cannot_prove_an_image_unused() {
        let containers = serde_json::to_vec(&json!([{"configuration": {
            "image": {"descriptor": {"digest": format!("sha256:{}", "a".repeat(64))}}
        }}]))
        .unwrap();
        let id = ImageId::parse("opaque-runtime-id").unwrap();
        assert!(container_image_in_use(&containers, &Target::Id(id.clone()), Some(&id)).is_err());
    }

    #[test]
    fn apple_container_list_preserves_images_used_by_existing_containers() {
        let output = serde_json::to_vec(&json!([{
            "configuration": {
                "image": {
                    "descriptor": {
                        "digest": format!("sha256:{}", "c".repeat(64))
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
                Some(&ImageId::parse(&"c".repeat(64)).expect("valid ID"))
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
                Some(&ImageId::parse(&"d".repeat(64)).expect("valid ID"))
            )
            .expect("parse container list")
        );
    }
}
