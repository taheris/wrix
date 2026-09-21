use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use serde_json::json;
use wrix_sandbox::image::{
    self, Digest, ImageId, ImageRef, ImageRow, InstallRequest, Layer, OciSource, Runtime, Source,
    SourceKind, Store, Target,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn digest_preflight_skips_source_execution_on_hit() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("image-digest-hit")
        .tempdir()?;
    let digest = digest('a');
    let missing_source = root.path().join("source-that-must-not-run");
    let mut store = FakeStore::default();
    store.present_digests.insert(digest.clone());

    image::install(
        &mut store,
        &InstallRequest::new(
            Runtime::Podman,
            &ImageRef::parse("localhost/wrix-hit:test")?,
            &Source::parse(&missing_source, SourceKind::NixDescriptor)?,
            Some(&digest),
        )?,
    )?;

    assert!(!missing_source.exists());
    assert!(store.copy_calls().is_empty());
    assert!(!store.loaded_archive());
    Ok(())
}

#[test]
fn digest_preflight_tags_matching_store_reference_as_selected_ref() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("image-digest-retag")
        .tempdir()?;
    let digest = digest('9');
    let missing_source = root.path().join("source-that-must-not-run");
    let mut store = FakeStore {
        digest_source: Some(Target::Reference(ImageRef::parse("wrix-existing:old")?)),
        ..FakeStore::default()
    };
    store.present_digests.insert(digest.clone());

    image::install(
        &mut store,
        &InstallRequest::new(
            Runtime::Container,
            &ImageRef::parse("wrix-selected:live")?,
            &Source::parse(&missing_source, SourceKind::DockerArchive)?,
            Some(&digest),
        )?,
    )?;

    assert_eq!(
        store.calls,
        vec![Call::Tag {
            source: String::from("wrix-existing:old"),
            target: String::from("wrix-selected:live"),
        }]
    );
    assert!(!missing_source.exists());
    Ok(())
}

#[test]
fn linux_descriptor_sources_use_archiveless_install_path() -> TestResult {
    let root = tempfile::Builder::new().prefix("image-oci").tempdir()?;
    let digest = digest('b');
    let layout = root.path().join("oci-layout");
    let descriptor = write_descriptor(
        root.path(),
        "descriptor.json",
        &layout,
        &digest,
        &[layer('1', 11), layer('2', 13)],
    )?;
    let mut store = FakeStore::default();

    image::install(
        &mut store,
        &InstallRequest::new(
            Runtime::Podman,
            &ImageRef::parse("localhost/wrix-oci:test")?,
            &Source::parse(&descriptor, SourceKind::NixDescriptor)?,
            Some(&digest),
        )?,
    )?;

    assert_eq!(
        store.copy_calls(),
        vec![Call::CopyOci {
            source: format!("oci:{}:latest", layout.display()),
            destination: String::from("containers-storage:localhost/wrix-oci:test"),
        }]
    );
    assert!(!store.archive_copy_used());
    assert!(!store.loaded_archive());
    Ok(())
}

#[test]
fn descriptor_digest_preflight_works_without_profile_digest() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("descriptor-digest")
        .tempdir()?;
    let desired = digest('d');
    let descriptor = write_descriptor(
        root.path(),
        "image.json",
        &root.path().join("missing-layout"),
        &desired,
        &[],
    )?;
    let mut store = FakeStore::default();
    store.present_digests.insert(desired);
    image::install(
        &mut store,
        &InstallRequest::new(
            Runtime::Podman,
            &ImageRef::parse("localhost/wrix-descriptor:test")?,
            &Source::parse(&descriptor, SourceKind::NixDescriptor)?,
            None,
        )?,
    )?;
    assert!(store.copy_calls().is_empty());
    assert!(!store.loaded_archive());
    Ok(())
}

#[test]
fn already_loaded_image_performs_no_store_writes() -> TestResult {
    let root = tempfile::Builder::new().prefix("image-loaded").tempdir()?;
    let digest = digest('c');
    let layout = root.path().join("oci-layout");
    let descriptor = write_descriptor(
        root.path(),
        "descriptor.json",
        &layout,
        &digest,
        &[layer('1', 11), layer('2', 13)],
    )?;
    let mut store = FakeStore::default();
    let source = Source::parse(&descriptor, SourceKind::NixDescriptor)?;
    let reference = ImageRef::parse("localhost/wrix-loaded:test")?;
    let request = InstallRequest::new(Runtime::Podman, &reference, &source, Some(&digest))?;

    image::install(&mut store, &request)?;
    assert_eq!(store.copy_calls().len(), 1);
    fs::remove_file(&descriptor)?;
    store.clear_observations();

    image::install(&mut store, &request)?;

    assert!(store.copy_calls().is_empty());
    assert!(!store.loaded_archive());
    Ok(())
}

#[test]
fn darwin_docker_archive_sources_tag_loaded_image() -> TestResult {
    let root = tempfile::Builder::new().prefix("image-darwin").tempdir()?;
    let archive = root.path().join("image.tar");
    fs::write(&archive, b"fake archive")?;
    let desired_digest = digest('f');
    let loaded_ref = format!("untagged@{}", digest('0').as_str());
    let mut store = FakeStore {
        loaded_archive_ref: Some(ImageRef::parse(&loaded_ref)?),
        ..FakeStore::default()
    };

    image::install(
        &mut store,
        &InstallRequest::new(
            Runtime::Container,
            &ImageRef::parse("wrix-darwin:test")?,
            &Source::parse(&archive, SourceKind::DockerArchive)?,
            Some(&desired_digest),
        )?,
    )?;

    assert_eq!(
        store.calls,
        vec![
            Call::LoadArchive {
                archive: archive.display().to_string(),
            },
            Call::Tag {
                source: loaded_ref,
                target: String::from("wrix-darwin:test"),
            },
            Call::Delete {
                target: format!("untagged@{}", digest('0').as_str()),
            },
        ]
    );
    assert!(!store.archive_copy_used());
    assert!(store.copy_calls().is_empty());
    Ok(())
}

#[test]
fn darwin_tag_failure_preserves_temporary_image() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("image-darwin-tag-failure")
        .tempdir()?;
    let archive = root.path().join("image.tar");
    fs::write(&archive, b"fake archive")?;
    let loaded_ref = format!("untagged@{}", digest('1').as_str());
    let mut store = FakeStore {
        loaded_archive_ref: Some(ImageRef::parse(&loaded_ref)?),
        tag_error: true,
        ..FakeStore::default()
    };

    let result = image::install(
        &mut store,
        &InstallRequest::new(
            Runtime::Container,
            &ImageRef::parse("wrix-darwin:test")?,
            &Source::parse(&archive, SourceKind::DockerArchive)?,
            Some(&digest('e')),
        )?,
    );

    assert!(result.is_err());
    assert_eq!(
        store.calls,
        vec![
            Call::LoadArchive {
                archive: archive.display().to_string(),
            },
            Call::Tag {
                source: loaded_ref,
                target: String::from("wrix-darwin:test"),
            },
        ]
    );
    Ok(())
}

#[test]
fn darwin_delete_failure_stops_install() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("image-darwin-delete-failure")
        .tempdir()?;
    let archive = root.path().join("image.tar");
    fs::write(&archive, b"fake archive")?;
    let loaded_ref = format!("untagged@{}", digest('2').as_str());
    let mut store = FakeStore {
        loaded_archive_ref: Some(ImageRef::parse(&loaded_ref)?),
        delete_error: true,
        ..FakeStore::default()
    };

    let result = image::install(
        &mut store,
        &InstallRequest::new(
            Runtime::Container,
            &ImageRef::parse("wrix-darwin:test")?,
            &Source::parse(&archive, SourceKind::DockerArchive)?,
            Some(&digest('d')),
        )?,
    );

    assert!(result.is_err());
    assert_eq!(
        store.calls,
        vec![
            Call::LoadArchive {
                archive: archive.display().to_string(),
            },
            Call::Tag {
                source: loaded_ref.clone(),
                target: String::from("wrix-darwin:test"),
            },
            Call::Delete { target: loaded_ref },
        ]
    );
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Call {
    Tag {
        source: String,
        target: String,
    },
    CopyOci {
        source: String,
        destination: String,
    },
    CopyArchive {
        archive: String,
        destination: String,
    },
    LoadArchive {
        archive: String,
    },
    Delete {
        target: String,
    },
}

#[derive(Default)]
struct FakeStore {
    present_digests: BTreeSet<Digest>,
    calls: Vec<Call>,
    docker_archive_digest: Option<Digest>,
    loaded_archive_ref: Option<ImageRef>,
    digest_source: Option<Target>,
    tag_error: bool,
    delete_error: bool,
}

impl FakeStore {
    fn clear_observations(&mut self) {
        self.calls.clear();
    }

    fn copy_calls(&self) -> Vec<Call> {
        self.calls
            .iter()
            .filter(|call| matches!(call, Call::CopyOci { .. }))
            .cloned()
            .collect()
    }

    fn archive_copy_used(&self) -> bool {
        self.calls
            .iter()
            .any(|call| matches!(call, Call::CopyArchive { .. }))
    }

    fn loaded_archive(&self) -> bool {
        self.calls
            .iter()
            .any(|call| matches!(call, Call::LoadArchive { .. }))
    }
}

impl Store for FakeStore {
    fn image_for_digest(
        &mut self,
        _runtime: Runtime,
        digest: &Digest,
    ) -> Result<Option<Target>, image::Error> {
        if !self.present_digests.contains(digest) {
            return Ok(None);
        }
        Ok(Some(
            self.digest_source
                .clone()
                .unwrap_or_else(|| Target::Digest(digest.clone())),
        ))
    }

    fn tag(
        &mut self,
        _runtime: Runtime,
        source: &Target,
        target: &ImageRef,
    ) -> Result<(), image::Error> {
        self.calls.push(Call::Tag {
            source: source.as_str().to_owned(),
            target: target.as_str().to_owned(),
        });
        if self.tag_error {
            return Err(io::Error::other("tag failed").into());
        }
        Ok(())
    }

    fn copy_oci_layout(
        &mut self,
        source: &OciSource,
        destination: &ImageRef,
    ) -> Result<(), image::Error> {
        self.calls.push(Call::CopyOci {
            source: format!("oci:{}:{}", source.layout, source.reference),
            destination: format!("containers-storage:{}", destination.as_str()),
        });
        self.present_digests.insert(source.digest.clone());
        Ok(())
    }

    fn copy_docker_archive(
        &mut self,
        archive: &Path,
        destination: &ImageRef,
    ) -> Result<(), image::Error> {
        self.calls.push(Call::CopyArchive {
            archive: archive.display().to_string(),
            destination: format!("containers-storage:{}", destination.as_str()),
        });
        Ok(())
    }

    fn load_docker_archive(&mut self, archive: &Path) -> Result<Option<ImageRef>, image::Error> {
        self.calls.push(Call::LoadArchive {
            archive: archive.display().to_string(),
        });
        if let Some(digest) = &self.docker_archive_digest {
            self.present_digests.insert(digest.clone());
        }
        Ok(self.loaded_archive_ref.clone())
    }

    fn docker_archive_config_digest(
        &mut self,
        _archive: &Path,
    ) -> Result<Option<Digest>, image::Error> {
        Ok(self.docker_archive_digest.clone())
    }

    fn image_rows(&mut self, _runtime: Runtime) -> Result<Vec<ImageRow>, image::Error> {
        Ok(Vec::new())
    }

    fn image_id(
        &mut self,
        _runtime: Runtime,
        _target: &Target,
    ) -> Result<Option<ImageId>, image::Error> {
        Ok(None)
    }

    fn image_digest(
        &mut self,
        _runtime: Runtime,
        _target: &Target,
    ) -> Result<Option<Digest>, image::Error> {
        Ok(None)
    }

    fn image_managed(&mut self, _runtime: Runtime, _target: &Target) -> Result<bool, image::Error> {
        Ok(false)
    }

    fn image_in_use(&mut self, _runtime: Runtime, _target: &Target) -> Result<bool, image::Error> {
        Ok(false)
    }

    fn delete_image(&mut self, _runtime: Runtime, target: &Target) -> Result<(), image::Error> {
        self.calls.push(Call::Delete {
            target: target.as_str().to_owned(),
        });
        if self.delete_error {
            return Err(io::Error::other("delete failed").into());
        }
        Ok(())
    }
}

fn write_descriptor(
    root: &Path,
    name: &str,
    layout: &Path,
    digest: &Digest,
    layers: &[Layer],
) -> io::Result<PathBuf> {
    fs::create_dir_all(layout)?;
    let path = root.join(name);
    let layer_values = layers
        .iter()
        .map(|layer| json!({ "digest": layer.digest, "size": layer.size }))
        .collect::<Vec<_>>();
    let descriptor = serde_json::to_vec(&json!({
        "schema": 1,
        "source_kind": "nix-descriptor",
        "digest": digest,
        "oci_layout": layout.display().to_string(),
        "oci_ref": "latest",
        "layers": layer_values,
    }))
    .map_err(io::Error::other)?;
    fs::write(&path, descriptor)?;
    Ok(path)
}

fn layer(ch: char, size: u64) -> Layer {
    Layer {
        digest: digest(ch),
        size,
    }
}

fn digest(ch: char) -> Digest {
    let value = format!("sha256:{}", ch.to_string().repeat(64));
    let Ok(digest) = Digest::parse(&value) else {
        std::process::abort();
    };
    digest
}
