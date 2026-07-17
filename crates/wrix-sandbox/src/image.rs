use std::{
    collections::BTreeSet,
    env, fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Output, Stdio},
};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Runtime {
    Podman,
    Container,
}

#[derive(Clone, Debug)]
pub struct InstallRequest<'a> {
    pub runtime: Runtime,
    pub image_ref: &'a str,
    pub image_source: &'a str,
    pub source_kind: SourceKind,
    pub digest: &'a str,
}

#[derive(Clone, Debug)]
pub struct RetentionRequest<'a> {
    pub runtime: Runtime,
    pub image_ref: &'a str,
    pub image_source: &'a str,
    pub source_kind: SourceKind,
    pub digest: &'a str,
    pub mru_path: &'a Path,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OciSource {
    pub digest: String,
    pub layout: String,
    pub reference: String,
    pub layers: Vec<Layer>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Layer {
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Record {
    #[serde(default, rename = "ref", skip_serializing_if = "String::is_empty")]
    pub ref_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub digest: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
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
    Io { source: io::Error },
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

pub trait Store {
    fn digest_present(&mut self, runtime: Runtime, digest: &str) -> Result<bool, Error>;

    fn tag(&mut self, runtime: Runtime, source: &str, target: &str) -> Result<(), Error>;

    fn linux_store_ref(&mut self, image_ref: &str) -> Result<String, Error>;

    fn copy_oci_layout(&mut self, source: &OciSource, destination: &str) -> Result<(), Error>;

    fn copy_docker_archive(&mut self, archive: &str, destination: &str) -> Result<(), Error>;

    fn load_docker_archive(&mut self, archive: &str) -> Result<Option<String>, Error>;

    fn docker_archive_config_digest(&mut self, archive: &str) -> Result<Option<String>, Error>;

    fn image_rows(&mut self, runtime: Runtime) -> Result<Vec<String>, Error>;

    fn image_id(&mut self, runtime: Runtime, target: &str) -> Result<Option<String>, Error>;

    fn image_digest(&mut self, runtime: Runtime, target: &str) -> Result<Option<String>, Error>;

    fn image_managed(&mut self, runtime: Runtime, target: &str) -> Result<bool, Error>;

    fn image_in_use(&mut self, runtime: Runtime, target: &str) -> Result<bool, Error>;

    fn delete_image(&mut self, runtime: Runtime, target: &str) -> Result<(), Error>;
}

#[derive(Default)]
pub struct CommandStore;

pub fn install(store: &mut impl Store, request: &InstallRequest<'_>) -> Result<(), Error> {
    if request.image_source.is_empty() {
        return Ok(());
    }
    let desired = desired_digest(
        store,
        request.image_source,
        request.source_kind,
        request.digest,
    )?;
    if !desired.is_empty() && store.digest_present(request.runtime, &desired)? {
        if request.runtime == Runtime::Podman {
            store.tag(request.runtime, &desired, request.image_ref)?;
        }
        return Ok(());
    }

    match (request.runtime, request.source_kind) {
        (Runtime::Podman, SourceKind::NixDescriptor) => {
            let descriptor = read_descriptor(request.image_source)?;
            let source = descriptor.oci_source(request.image_source)?;
            let store_ref = store.linux_store_ref(request.image_ref)?;
            store.copy_oci_layout(&source, &store_ref)?;
        }
        (Runtime::Podman, SourceKind::DockerArchive) => {
            let store_ref = store.linux_store_ref(request.image_ref)?;
            store.copy_docker_archive(request.image_source, &store_ref)?;
        }
        (Runtime::Container, SourceKind::DockerArchive) => {
            if let Some(untagged) = store.load_docker_archive(request.image_source)? {
                store.tag(request.runtime, &untagged, request.image_ref)?;
            }
        }
        (Runtime::Container, kind) => {
            return Err(Error::UnsupportedSourceKind {
                kind: kind.as_str(),
            });
        }
    }

    if request.runtime == Runtime::Podman
        && let Some(repo) = request.image_ref.rsplit_once(':').map(|(repo, _tag)| repo)
    {
        let latest = format!("{repo}:latest");
        store.tag(request.runtime, request.image_ref, &latest)?;
    }
    Ok(())
}

pub fn remember_and_prune(
    store: &mut impl Store,
    request: &RetentionRequest<'_>,
) -> Result<(), Error> {
    let digest = if request.image_source.is_empty() {
        String::new()
    } else {
        desired_digest(
            store,
            request.image_source,
            request.source_kind,
            request.digest,
        )?
    };
    remember(
        store,
        request.runtime,
        request.mru_path,
        request.image_ref,
        &digest,
    )?;
    prune(
        store,
        request.runtime,
        request.mru_path,
        request.image_ref,
        &digest,
    )
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
    image_source: &str,
    kind: SourceKind,
    digest: &str,
) -> Result<String, Error> {
    if digest.starts_with("sha256:") {
        return Ok(digest.to_owned());
    }
    let path = Path::new(digest);
    if path.is_file() {
        let value = fs::read_to_string(path)?;
        let trimmed = value.trim();
        if trimmed.starts_with("sha256:") {
            return Ok(trimmed.to_owned());
        }
    }
    match kind {
        SourceKind::NixDescriptor => {
            let descriptor = read_descriptor(image_source)?;
            if descriptor.digest.starts_with("sha256:") {
                Ok(descriptor.digest)
            } else {
                Err(Error::MissingDescriptorDigest {
                    path: image_source.to_owned(),
                })
            }
        }
        SourceKind::DockerArchive => store
            .docker_archive_config_digest(image_source)?
            .filter(|value| value.starts_with("sha256:"))
            .ok_or_else(|| Error::MissingDockerArchiveDigest {
                path: image_source.to_owned(),
            }),
    }
}

fn remember(
    store: &mut impl Store,
    runtime: Runtime,
    path: &Path,
    image_ref: &str,
    digest: &str,
) -> Result<(), Error> {
    if image_ref.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut records = read_records(path)?;
    records.insert(
        0,
        Record {
            ref_name: image_ref.to_owned(),
            digest: digest.to_owned(),
            id: store
                .image_id(runtime, image_ref)?
                .unwrap_or_else(String::new),
        },
    );
    let mut seen = BTreeSet::new();
    let records = records
        .into_iter()
        .filter(|record| {
            !record.ref_name.is_empty() || !record.digest.is_empty() || !record.id.is_empty()
        })
        .filter(|record| {
            seen.insert(format!(
                "{}\0{}\0{}",
                record.ref_name, record.digest, record.id
            ))
        })
        .take(8)
        .collect::<Vec<_>>();
    let json =
        serde_json::to_string_pretty(&records).map_err(|source| Error::MruJson { source })?;
    fs::write(path, format!("{json}\n"))?;
    Ok(())
}

fn prune(
    store: &mut impl Store,
    runtime: Runtime,
    mru_path: &Path,
    image_ref: &str,
    digest: &str,
) -> Result<(), Error> {
    let mut keep_refs = BTreeSet::new();
    let mut keep_ids = BTreeSet::new();
    let mut keep_digests = BTreeSet::new();
    add_normalized(&mut keep_refs, image_ref);
    add_normalized(&mut keep_digests, digest);
    if let Some(id) = store.image_id(runtime, image_ref)? {
        add_normalized(&mut keep_ids, &id);
    }
    if let Some(actual_digest) = store.image_digest(runtime, image_ref)? {
        add_normalized(&mut keep_digests, &actual_digest);
    }
    for record in read_records(mru_path)? {
        add_normalized(&mut keep_refs, &record.ref_name);
        add_normalized(&mut keep_ids, &record.id);
        add_normalized(&mut keep_digests, &record.digest);
    }
    for image in list_images(store, runtime)? {
        if !image.managed && !image.legacy {
            continue;
        }
        if keep_refs.contains(&image.ref_name)
            || keep_ids.contains(&image.id)
            || keep_digests.contains(&image.digest)
        {
            continue;
        }
        if store.image_in_use(runtime, &image.target)? {
            continue;
        }
        store.delete_image(runtime, &image.target)?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct Descriptor {
    #[serde(default)]
    digest: String,
    #[serde(default)]
    oci_layout: String,
    #[serde(default)]
    oci_ref: Option<String>,
    #[serde(default)]
    layers: Vec<Layer>,
}

impl Descriptor {
    fn oci_source(self, path: &str) -> Result<OciSource, Error> {
        if self.oci_layout.is_empty() {
            return Err(Error::MissingDescriptorLayout {
                path: path.to_owned(),
            });
        }
        Ok(OciSource {
            digest: self.digest,
            layout: self.oci_layout,
            reference: self.oci_ref.unwrap_or_else(|| String::from("latest")),
            layers: self.layers,
        })
    }
}

struct ListedImage {
    ref_name: String,
    target: String,
    id: String,
    digest: String,
    managed: bool,
    legacy: bool,
}

fn read_descriptor(path: &str) -> Result<Descriptor, Error> {
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
            let mut sink = io::stderr().lock();
            writeln!(
                sink,
                "wrix: resetting invalid image MRU {}: {source}",
                path.display()
            )?;
            Ok(Vec::new())
        }
    }
}

fn list_images(store: &mut impl Store, runtime: Runtime) -> Result<Vec<ListedImage>, Error> {
    let mut images = Vec::new();
    for row in store.image_rows(runtime)? {
        if let Some(image) = listed_image_from_line(store, runtime, &row)? {
            images.push(image);
        }
    }
    Ok(images)
}

fn listed_image_from_line(
    store: &mut impl Store,
    runtime: Runtime,
    line: &str,
) -> Result<Option<ListedImage>, Error> {
    let mut fields = line.split_whitespace();
    let Some(repo) = fields.next() else {
        return Ok(None);
    };
    if runtime == Runtime::Container && repo.eq_ignore_ascii_case("repository") {
        return Ok(None);
    }
    let tag = fields.next().unwrap_or("<none>");
    let listed_id = fields.next().unwrap_or("");
    let ref_name = if repo != "<none>" && tag != "<none>" {
        format!("{repo}:{tag}")
    } else {
        String::new()
    };
    let target = normalized_value(&ref_name)
        .or_else(|| normalized_value(listed_id))
        .unwrap_or_default();
    if target.is_empty() {
        return Ok(None);
    }
    let id = store
        .image_id(runtime, &target)?
        .unwrap_or_else(|| listed_id.to_owned());
    let digest = store
        .image_digest(runtime, &target)?
        .unwrap_or_else(String::new);
    let managed = store.image_managed(runtime, &target)?;
    let legacy = match runtime {
        Runtime::Podman => ref_name.starts_with("localhost/wrix-"),
        Runtime::Container => ref_name.starts_with("wrix-"),
    };
    Ok(Some(ListedImage {
        ref_name,
        target,
        id,
        digest,
        managed,
        legacy,
    }))
}

fn add_normalized(set: &mut BTreeSet<String>, value: &str) {
    if let Some(value) = normalized_value(value) {
        set.insert(value);
    }
}

fn normalized_value(value: &str) -> Option<String> {
    match value.trim() {
        "" | "<none>" | "<no value>" | "null" => None,
        value => Some(value.to_owned()),
    }
}

impl Store for CommandStore {
    fn digest_present(&mut self, runtime: Runtime, digest: &str) -> Result<bool, Error> {
        match runtime {
            Runtime::Podman => Ok(run_output(
                "podman",
                &["image", "inspect", "--format", "{{.Id}}", digest],
            )
            .is_ok_and(|output| output.status.success())),
            Runtime::Container => darwin_digest_present(digest),
        }
    }

    fn tag(&mut self, runtime: Runtime, source: &str, target: &str) -> Result<(), Error> {
        let output = match runtime {
            Runtime::Podman => run_output("podman", &["tag", source, target])?,
            Runtime::Container => run_output("container", &["image", "tag", source, target])?,
        };
        if output.status.success() {
            return Ok(());
        }
        let mut sink = io::stderr().lock();
        writeln!(
            sink,
            "wrix: could not tag image {source} as {target}: {}",
            String::from_utf8_lossy(&output.stderr)
        )?;
        Ok(())
    }

    fn linux_store_ref(&mut self, image_ref: &str) -> Result<String, Error> {
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
        Ok(store_ref)
    }

    fn copy_oci_layout(&mut self, source: &OciSource, destination: &str) -> Result<(), Error> {
        run_required(
            "skopeo",
            &[
                "--insecure-policy",
                "copy",
                "--quiet",
                &format!("oci:{}:{}", source.layout, source.reference),
                destination,
            ],
        )
    }

    fn copy_docker_archive(&mut self, archive: &str, destination: &str) -> Result<(), Error> {
        run_required(
            "skopeo",
            &[
                "--insecure-policy",
                "copy",
                "--quiet",
                &format!("docker-archive:{archive}"),
                destination,
            ],
        )
    }

    fn load_docker_archive(&mut self, archive: &str) -> Result<Option<String>, Error> {
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

    fn docker_archive_config_digest(&mut self, archive: &str) -> Result<Option<String>, Error> {
        let output = run_required_output(
            "skopeo",
            &["inspect", "--raw", &format!("docker-archive:{archive}")],
        )?;
        let value = serde_json::from_slice::<Value>(&output.stdout)
            .map_err(|source| Error::DescriptorJson { source })?;
        Ok(value
            .pointer("/config/digest")
            .and_then(Value::as_str)
            .and_then(normalized_value))
    }

    fn image_rows(&mut self, runtime: Runtime) -> Result<Vec<String>, Error> {
        let output = match runtime {
            Runtime::Podman => run_required_output(
                "podman",
                &["images", "--format", "{{.Repository}} {{.Tag}} {{.ID}}"],
            )?,
            Runtime::Container => run_required_output("container", &["image", "list"])?,
        };
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(ToOwned::to_owned)
            .collect())
    }

    fn image_id(&mut self, runtime: Runtime, target: &str) -> Result<Option<String>, Error> {
        inspect_value(runtime, target, InspectField::Id)
    }

    fn image_digest(&mut self, runtime: Runtime, target: &str) -> Result<Option<String>, Error> {
        inspect_value(runtime, target, InspectField::Digest)
    }

    fn image_managed(&mut self, runtime: Runtime, target: &str) -> Result<bool, Error> {
        Ok(inspect_value(runtime, target, InspectField::Managed)?.as_deref() == Some("true"))
    }

    fn image_in_use(&mut self, runtime: Runtime, target: &str) -> Result<bool, Error> {
        if runtime != Runtime::Podman {
            return Ok(false);
        }
        let filter = format!("ancestor={target}");
        let output = run_output(
            "podman",
            &["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"],
        )?;
        Ok(output.status.success() && !trim_stdout(&output.stdout).is_empty())
    }

    fn delete_image(&mut self, runtime: Runtime, target: &str) -> Result<(), Error> {
        let output = match runtime {
            Runtime::Podman => run_output("podman", &["rmi", target])?,
            Runtime::Container => run_output("container", &["image", "delete", target])?,
        };
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut sink = io::stderr().lock();
        if stderr.contains("in use") || stderr.contains("is using") {
            writeln!(
                sink,
                "prune-stale-images: {target} pinned by a container — upgrades on next start"
            )?;
        } else {
            writeln!(
                sink,
                "prune-stale-images: could not remove {target}: {stderr}"
            )?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum InspectField {
    Id,
    Digest,
    Managed,
}

fn inspect_value(
    runtime: Runtime,
    target: &str,
    field: InspectField,
) -> Result<Option<String>, Error> {
    if target.is_empty() {
        return Ok(None);
    }
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
    Ok(value
        .pointer(pointer)
        .or_else(|| match field {
            InspectField::Managed => value.pointer("/0/Labels/wrix.managed"),
            _ => None,
        })
        .and_then(Value::as_str)
        .and_then(normalized_value))
}

fn darwin_digest_present(digest: &str) -> Result<bool, Error> {
    let Ok(output) = run_output("container", &["image", "list"]) else {
        return Ok(false);
    };
    if !output.status.success() {
        return Ok(false);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines().skip(1) {
        let Some(reference) = container_reference_from_line(line) else {
            continue;
        };
        let inspect = run_output("container", &["image", "inspect", &reference])?;
        if !inspect.status.success() {
            continue;
        }
        let value = serde_json::from_slice::<Value>(&inspect.stdout)
            .map_err(|source| Error::DescriptorJson { source })?;
        let actual = value
            .pointer("/0/digest")
            .or_else(|| value.pointer("/0/id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if actual.trim_start_matches("sha256:") == digest.trim_start_matches("sha256:") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn container_reference_from_line(line: &str) -> Option<String> {
    let mut fields = line.split_whitespace();
    let repo = fields.next()?;
    if repo.eq_ignore_ascii_case("repository") {
        return None;
    }
    let tag = fields.next()?;
    Some(format!("{repo}:{tag}"))
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

fn load_container_archive(archive: &str, temp_dir: &Path) -> Result<Option<String>, Error> {
    let oci_archive = temp_dir.join("image.oci");
    let source = format!("docker-archive:{archive}");
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
    Ok(loaded_container_ref(&output.stdout, &output.stderr))
}

fn loaded_container_ref(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    for token in text.split_whitespace() {
        let token = token.trim_matches(|ch| matches!(ch, '"' | '\'' | ',' | ';'));
        let Some(rest) = token.strip_prefix("untagged@sha256:") else {
            continue;
        };
        let digest = rest
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect::<String>();
        if !digest.is_empty() {
            return Some(format!("untagged@sha256:{digest}"));
        }
    }
    None
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
    use super::loaded_container_ref;

    #[test]
    fn apple_load_output_parser_extracts_untagged_ref() {
        let output = b"loading\nLoaded: untagged@sha256:abcdef0123456789, done\n";

        assert_eq!(
            loaded_container_ref(output, b""),
            Some(String::from("untagged@sha256:abcdef0123456789"))
        );
    }
}
