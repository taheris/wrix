use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use serde_json::json;
use wrix_sandbox::image::{
    self, Digest, ImageId, ImageRef, ImageRow, OciSource, RetentionRequest, Runtime, Source,
    SourceKind, Store, Target,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn cleanup_prunes_only_wrix_managed_images_outside_bounded_keep_set() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("image-retention")
        .tempdir()?;
    let mru_path = root.path().join("image-mru.json");
    fs::write(
        &mru_path,
        serde_json::to_vec(&json!([
            {"ref": "localhost/wrix-recent-by-ref:old"},
            {"digest": digest('d')},
            {"id": "recent-id"},
            {"ref": "localhost/wrix-filler-1:old"},
            {"ref": "localhost/wrix-filler-2:old"},
            {"ref": "localhost/wrix-filler-3:old"},
            {"ref": "localhost/wrix-filler-4:old"},
            {"ref": "localhost/wrix-dropped-by-bound:old"}
        ]))?,
    )?;
    let mut store = FakeStore::with_images(vec![
        fake_image("localhost/wrix-current:live", "current-id")
            .with_digest(digest('c').as_str())
            .managed(),
        fake_image("localhost/wrix-recent-by-ref:old", "recent-ref-id").managed(),
        fake_image("localhost/wrix-recent-by-digest:old", "recent-digest-id")
            .with_digest(digest('d').as_str())
            .managed(),
        fake_image("localhost/wrix-recent-by-id:old", "recent-id").managed(),
        fake_image("localhost/wrix-used:old", "used-id")
            .managed()
            .in_use(),
        fake_image("localhost/wrix-stale:old", "stale-id").managed(),
        FakeImage::dangling("managed-dangling-id").managed(),
        FakeImage::dangling("dangling-id"),
        fake_image("docker.io/library/ubuntu:latest", "user-id"),
    ]);

    image::remember_and_prune(
        &mut store,
        &RetentionRequest {
            runtime: Runtime::Podman,
            image_ref: &ImageRef::parse("localhost/wrix-current:live")?,
            source: Some(&Source::parse(
                "digest-from-profile-config",
                SourceKind::NixDescriptor,
            )?),
            digest: Some(&digest('c')),
            mru_path: &mru_path,
        },
    )?;

    let records = serde_json::from_slice::<Vec<image::Record>>(&fs::read(&mru_path)?)?;
    assert_eq!(records.len(), 8);
    assert_eq!(
        records[0].ref_name.as_ref().map(image::ImageRef::as_str),
        Some("localhost/wrix-current:live")
    );
    assert_eq!(records[0].digest.as_ref(), Some(&digest('c')));
    assert_eq!(
        records[0].id.as_ref().map(image::ImageId::as_str),
        Some("current-id")
    );
    assert!(!records.iter().any(
        |record| record.ref_name.as_ref().map(image::ImageRef::as_str)
            == Some("localhost/wrix-dropped-by-bound:old")
    ));

    let deleted = store.deleted.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        deleted,
        BTreeSet::from([
            String::from("localhost/wrix-stale:old"),
            String::from("managed-dangling-id"),
        ])
    );
    for kept in [
        "localhost/wrix-current:live",
        "localhost/wrix-recent-by-ref:old",
        "localhost/wrix-recent-by-digest:old",
        "localhost/wrix-recent-by-id:old",
        "localhost/wrix-used:old",
        "dangling-id",
        "docker.io/library/ubuntu:latest",
    ] {
        assert!(!deleted.contains(kept));
    }
    Ok(())
}

#[test]
fn concurrent_mru_updates_preserve_each_workspace_record() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("image-retention-concurrent")
        .tempdir()?;
    let mru_path = Arc::new(root.path().join("image-mru.json"));
    fs::write(&*mru_path, "[]\n")?;
    let barrier = Arc::new(Barrier::new(3));
    let completed = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for index in 0..2 {
        let path = Arc::clone(&mru_path);
        let ready = Arc::clone(&barrier);
        let completed = Arc::clone(&completed);
        handles.push(thread::spawn(move || -> Result<(), image::Error> {
            let image_ref = format!("localhost/wrix-workspace-{index}:live");
            let image_id = format!("workspace-{index}-id");
            let mut store =
                FakeStore::with_images(vec![fake_image(&image_ref, &image_id).managed()]);
            ready.wait();
            let result = image::remember_and_prune(
                &mut store,
                &RetentionRequest {
                    runtime: Runtime::Podman,
                    image_ref: &ImageRef::parse(&image_ref)?,
                    source: None,
                    digest: None,
                    mru_path: &path,
                },
            );
            completed.fetch_add(1, Ordering::Release);
            result
        }));
    }
    let reader_path = Arc::clone(&mru_path);
    let reader_ready = Arc::clone(&barrier);
    let reader_completed = Arc::clone(&completed);
    let reader = thread::spawn(move || -> Result<(), std::io::Error> {
        reader_ready.wait();
        while reader_completed.load(Ordering::Acquire) < 2 {
            serde_json::from_slice::<Vec<image::Record>>(&fs::read(&*reader_path)?)
                .map_err(std::io::Error::other)?;
            thread::yield_now();
        }
        Ok(())
    });
    for handle in handles {
        handle.join().map_err(|_| "retention worker panicked")??;
    }
    reader.join().map_err(|_| "retention reader panicked")??;

    let records = serde_json::from_slice::<Vec<image::Record>>(&fs::read(&*mru_path)?)?;
    assert_eq!(records.len(), 2);
    for index in 0..2 {
        let expected = format!("localhost/wrix-workspace-{index}:live");
        assert!(records.iter().any(
            |record| record.ref_name.as_ref().map(image::ImageRef::as_str)
                == Some(expected.as_str())
        ));
    }
    Ok(())
}

#[test]
fn container_cleanup_preserves_images_used_by_apple_containers() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("image-retention-container")
        .tempdir()?;
    let mru_path = root.path().join("image-mru.json");
    let mut store = FakeStore::with_images(vec![
        fake_image("wrix-current:live", "current-id").managed(),
        fake_image("wrix-running:old", "running-id")
            .managed()
            .in_use(),
    ]);

    image::remember_and_prune(
        &mut store,
        &RetentionRequest {
            runtime: Runtime::Container,
            image_ref: &ImageRef::parse("wrix-current:live")?,
            source: None,
            digest: None,
            mru_path: &mru_path,
        },
    )?;

    assert!(
        !store
            .deleted
            .iter()
            .any(|target| target == "wrix-running:old")
    );
    Ok(())
}

#[test]
fn container_cleanup_preserves_unlabelled_wrix_refs() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("image-retention-container-unlabelled")
        .tempdir()?;
    let mru_path = root.path().join("image-mru.json");
    let mut store = FakeStore::with_images(vec![
        fake_image("wrix-current:live", "current-id").managed(),
        fake_image("wrix-user-owned:old", "user-id"),
    ]);

    image::remember_and_prune(
        &mut store,
        &RetentionRequest {
            runtime: Runtime::Container,
            image_ref: &ImageRef::parse("wrix-current:live")?,
            source: None,
            digest: None,
            mru_path: &mru_path,
        },
    )?;

    assert!(
        !store
            .deleted
            .iter()
            .any(|target| target == "wrix-user-owned:old")
    );
    Ok(())
}

#[test]
fn fake_store_matches_podman_listing_contract() -> TestResult {
    let mut store = FakeStore::with_images(vec![
        fake_image("localhost/wrix-test:old", "image-id").with_digest(digest('a').as_str()),
        FakeImage::dangling("dangling-id"),
    ]);

    assert_eq!(
        store.image_rows(Runtime::Podman)?,
        vec![
            ImageRow {
                target: Target::Reference(ImageRef::parse("localhost/wrix-test:old")?),
                id: Some(ImageId::parse("image-id")?)
            },
            ImageRow {
                target: Target::Id(ImageId::parse("dangling-id")?),
                id: Some(ImageId::parse("dangling-id")?)
            },
        ]
    );
    assert_eq!(
        store.image_id(
            Runtime::Podman,
            &Target::Reference(ImageRef::parse("localhost/wrix-test:old")?)
        )?,
        Some(ImageId::parse("image-id")?)
    );
    assert_eq!(
        store.image_digest(Runtime::Podman, &Target::Id(ImageId::parse("image-id")?))?,
        Some(digest('a'))
    );
    Ok(())
}

#[derive(Clone, Debug)]
struct FakeImage {
    ref_name: Option<String>,
    id: String,
    digest: Option<String>,
    managed: bool,
    in_use: bool,
}

impl FakeImage {
    fn dangling(id: &str) -> Self {
        Self {
            ref_name: None,
            id: id.to_owned(),
            digest: None,
            managed: false,
            in_use: false,
        }
    }

    fn with_digest(mut self, digest: &str) -> Self {
        self.digest = Some(digest.to_owned());
        self
    }

    const fn managed(mut self) -> Self {
        self.managed = true;
        self
    }

    const fn in_use(mut self) -> Self {
        self.in_use = true;
        self
    }

    fn row(&self) -> Result<ImageRow, image::Error> {
        let id = ImageId::parse(&self.id)?;
        let reference = self.ref_name.as_deref().map(ImageRef::parse).transpose()?;
        Ok(ImageRow {
            target: reference.map_or_else(|| Target::Id(id.clone()), Target::Reference),
            id: Some(id),
        })
    }

    fn matches_target(&self, target: &str) -> bool {
        self.ref_name.as_deref() == Some(target) || self.id == target
    }
}

#[derive(Default)]
struct FakeStore {
    images: Vec<FakeImage>,
    present_digests: BTreeSet<Digest>,
    deleted: Vec<String>,
}

impl FakeStore {
    const fn with_images(images: Vec<FakeImage>) -> Self {
        Self {
            images,
            present_digests: BTreeSet::new(),
            deleted: Vec::new(),
        }
    }

    fn by_target(&self, target: &Target) -> Option<&FakeImage> {
        self.images
            .iter()
            .find(|image| image.matches_target(target.as_str()))
    }
}

impl Store for FakeStore {
    fn image_for_digest(
        &mut self,
        _runtime: Runtime,
        digest: &Digest,
    ) -> Result<Option<Target>, image::Error> {
        Ok(self
            .present_digests
            .contains(digest)
            .then(|| Target::Digest(digest.clone())))
    }

    fn tag(
        &mut self,
        _runtime: Runtime,
        _source: &Target,
        _target: &ImageRef,
    ) -> Result<(), image::Error> {
        Ok(())
    }

    fn copy_oci_layout(
        &mut self,
        source: &OciSource,
        _destination: &ImageRef,
    ) -> Result<(), image::Error> {
        self.present_digests.insert(source.digest.clone());
        Ok(())
    }

    fn copy_docker_archive(
        &mut self,
        _archive: &Path,
        _destination: &ImageRef,
    ) -> Result<(), image::Error> {
        Ok(())
    }
    fn load_docker_archive(&mut self, _archive: &Path) -> Result<Option<ImageRef>, image::Error> {
        Ok(None)
    }
    fn docker_archive_config_digest(
        &mut self,
        _archive: &Path,
    ) -> Result<Option<Digest>, image::Error> {
        Ok(None)
    }

    fn image_rows(&mut self, _runtime: Runtime) -> Result<Vec<ImageRow>, image::Error> {
        self.images.iter().map(FakeImage::row).collect()
    }

    fn image_id(
        &mut self,
        _runtime: Runtime,
        target: &Target,
    ) -> Result<Option<ImageId>, image::Error> {
        self.by_target(target)
            .map(|image| ImageId::parse(&image.id).map_err(image::Error::from))
            .transpose()
    }

    fn image_digest(
        &mut self,
        _runtime: Runtime,
        target: &Target,
    ) -> Result<Option<Digest>, image::Error> {
        self.by_target(target)
            .and_then(|image| image.digest.as_deref())
            .map(|digest| Digest::parse(digest).map_err(image::Error::from))
            .transpose()
    }

    fn image_managed(&mut self, _runtime: Runtime, target: &Target) -> Result<bool, image::Error> {
        Ok(self.by_target(target).is_some_and(|image| image.managed))
    }

    fn image_in_use(&mut self, _runtime: Runtime, target: &Target) -> Result<bool, image::Error> {
        Ok(self.by_target(target).is_some_and(|image| image.in_use))
    }

    fn delete_image(&mut self, _runtime: Runtime, target: &Target) -> Result<(), image::Error> {
        self.deleted.push(target.as_str().to_owned());
        Ok(())
    }
}

fn fake_image(ref_name: &str, id: &str) -> FakeImage {
    FakeImage {
        ref_name: Some(ref_name.to_owned()),
        id: id.to_owned(),
        digest: None,
        managed: false,
        in_use: false,
    }
}

fn digest(ch: char) -> Digest {
    let value = format!("sha256:{}", ch.to_string().repeat(64));
    let Ok(digest) = Digest::parse(&value) else {
        std::process::abort();
    };
    digest
}
