use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use serde_json::json;
use wrix_sandbox::image::{
    self, Digest, InstallRequest, Layer, OciSource, Runtime, SourceKind, Store,
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
        &InstallRequest {
            runtime: Runtime::Podman,
            image_ref: "localhost/wrix-hit:test",
            image_source: &missing_source.display().to_string(),
            source_kind: SourceKind::NixDescriptor,
            digest: Some(&digest),
        },
    )?;

    assert!(!missing_source.exists());
    assert!(store.copy_calls().is_empty());
    assert!(!store.loaded_archive());
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
        &InstallRequest {
            runtime: Runtime::Podman,
            image_ref: "localhost/wrix-oci:test",
            image_source: &descriptor.display().to_string(),
            source_kind: SourceKind::NixDescriptor,
            digest: Some(&digest),
        },
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
    let descriptor_source = descriptor.display().to_string();
    let request = InstallRequest {
        runtime: Runtime::Podman,
        image_ref: "localhost/wrix-loaded:test",
        image_source: &descriptor_source,
        source_kind: SourceKind::NixDescriptor,
        digest: Some(&digest),
    };

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
    let loaded_ref = format!("untagged@{}", digest('0'));
    let mut store = FakeStore {
        loaded_archive_ref: Some(loaded_ref.clone()),
        ..FakeStore::default()
    };

    image::install(
        &mut store,
        &InstallRequest {
            runtime: Runtime::Container,
            image_ref: "wrix-darwin:test",
            image_source: &archive.display().to_string(),
            source_kind: SourceKind::DockerArchive,
            digest: Some(&desired_digest),
        },
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
                target: format!("untagged@{}", digest('0')),
            },
        ]
    );
    assert!(!store.archive_copy_used());
    assert!(store.copy_calls().is_empty());
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
    docker_archive_digest: Option<String>,
    loaded_archive_ref: Option<String>,
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
    fn digest_present(&mut self, _runtime: Runtime, digest: &str) -> Result<bool, image::Error> {
        Ok(Digest::parse(digest).is_ok_and(|digest| self.present_digests.contains(&digest)))
    }

    fn tag(&mut self, _runtime: Runtime, source: &str, target: &str) -> Result<(), image::Error> {
        self.calls.push(Call::Tag {
            source: source.to_owned(),
            target: target.to_owned(),
        });
        Ok(())
    }

    fn linux_store_ref(&mut self, image_ref: &str) -> Result<String, image::Error> {
        Ok(format!("containers-storage:{image_ref}"))
    }

    fn copy_oci_layout(
        &mut self,
        source: &OciSource,
        destination: &str,
    ) -> Result<(), image::Error> {
        self.calls.push(Call::CopyOci {
            source: format!("oci:{}:{}", source.layout, source.reference),
            destination: destination.to_owned(),
        });
        self.present_digests.insert(source.digest.clone());
        Ok(())
    }

    fn copy_docker_archive(
        &mut self,
        archive: &str,
        destination: &str,
    ) -> Result<(), image::Error> {
        self.calls.push(Call::CopyArchive {
            archive: archive.to_owned(),
            destination: destination.to_owned(),
        });
        Ok(())
    }

    fn load_docker_archive(&mut self, archive: &str) -> Result<Option<String>, image::Error> {
        self.calls.push(Call::LoadArchive {
            archive: archive.to_owned(),
        });
        if let Some(digest) = &self.docker_archive_digest {
            if let Ok(digest) = Digest::parse(digest) {
                self.present_digests.insert(digest);
            }
        }
        Ok(self.loaded_archive_ref.clone())
    }

    fn docker_archive_config_digest(
        &mut self,
        _archive: &str,
    ) -> Result<Option<String>, image::Error> {
        Ok(self.docker_archive_digest.clone())
    }

    fn image_rows(&mut self, _runtime: Runtime) -> Result<Vec<String>, image::Error> {
        Ok(Vec::new())
    }

    fn image_id(
        &mut self,
        _runtime: Runtime,
        _target: &str,
    ) -> Result<Option<String>, image::Error> {
        Ok(None)
    }

    fn image_digest(
        &mut self,
        _runtime: Runtime,
        _target: &str,
    ) -> Result<Option<String>, image::Error> {
        Ok(None)
    }

    fn image_managed(&mut self, _runtime: Runtime, _target: &str) -> Result<bool, image::Error> {
        Ok(false)
    }

    fn image_in_use(&mut self, _runtime: Runtime, _target: &str) -> Result<bool, image::Error> {
        Ok(false)
    }

    fn delete_image(&mut self, _runtime: Runtime, target: &str) -> Result<(), image::Error> {
        self.calls.push(Call::Delete {
            target: target.to_owned(),
        });
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
