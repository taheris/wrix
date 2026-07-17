use std::{collections::BTreeSet, fs};

use wrix_sandbox::image::{self, OciSource, RetentionRequest, Runtime, SourceKind, Store};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn cleanup_prunes_only_wrix_managed_images_outside_bounded_keep_set() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("image-retention")
        .tempdir()?;
    let mru_path = root.path().join("image-mru.json");
    fs::write(
        &mru_path,
        serde_json::to_vec(&vec![
            image::Record {
                ref_name: String::from("localhost/wrix-recent-by-ref:old"),
                digest: String::new(),
                id: String::new(),
            },
            image::Record {
                ref_name: String::new(),
                digest: digest('r'),
                id: String::new(),
            },
            image::Record {
                ref_name: String::new(),
                digest: String::new(),
                id: String::from("recent-id"),
            },
            image::Record {
                ref_name: String::from("localhost/wrix-filler-1:old"),
                digest: String::new(),
                id: String::new(),
            },
            image::Record {
                ref_name: String::from("localhost/wrix-filler-2:old"),
                digest: String::new(),
                id: String::new(),
            },
            image::Record {
                ref_name: String::from("localhost/wrix-filler-3:old"),
                digest: String::new(),
                id: String::new(),
            },
            image::Record {
                ref_name: String::from("localhost/wrix-filler-4:old"),
                digest: String::new(),
                id: String::new(),
            },
            image::Record {
                ref_name: String::from("localhost/wrix-dropped-by-bound:old"),
                digest: String::new(),
                id: String::new(),
            },
        ])?,
    )?;
    let mut store = FakeStore::with_images(vec![
        fake_image("localhost/wrix-current:live", "current-id")
            .with_digest(&digest('c'))
            .managed(),
        fake_image("localhost/wrix-recent-by-ref:old", "recent-ref-id").managed(),
        fake_image("localhost/wrix-recent-by-digest:old", "recent-digest-id")
            .with_digest(&digest('r'))
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
            image_ref: "localhost/wrix-current:live",
            image_source: "digest-from-profile-config",
            source_kind: SourceKind::NixDescriptor,
            digest: &digest('c'),
            mru_path: &mru_path,
        },
    )?;

    let records = serde_json::from_slice::<Vec<image::Record>>(&fs::read(&mru_path)?)?;
    assert_eq!(records.len(), 8);
    assert_eq!(records[0].ref_name, "localhost/wrix-current:live");
    assert_eq!(records[0].digest, digest('c'));
    assert_eq!(records[0].id, "current-id");
    assert!(
        !records
            .iter()
            .any(|record| record.ref_name == "localhost/wrix-dropped-by-bound:old")
    );

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
fn fake_store_matches_podman_listing_contract() -> TestResult {
    let mut store = FakeStore::with_images(vec![
        fake_image("localhost/wrix-test:old", "image-id").with_digest(&digest('a')),
        FakeImage::dangling("dangling-id"),
    ]);

    assert_eq!(
        store.image_rows(Runtime::Podman)?,
        vec![
            String::from("localhost/wrix-test old image-id"),
            String::from("<none> <none> dangling-id"),
        ]
    );
    assert_eq!(
        store.image_id(Runtime::Podman, "localhost/wrix-test:old")?,
        Some(String::from("image-id"))
    );
    assert_eq!(
        store.image_digest(Runtime::Podman, "image-id")?,
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

    fn row(&self) -> String {
        self.ref_name.as_ref().map_or_else(
            || format!("<none> <none> {}", self.id),
            |ref_name| {
                let (repo, tag) = ref_name
                    .rsplit_once(':')
                    .map_or((ref_name.as_str(), "latest"), |(repo, tag)| (repo, tag));
                format!("{repo} {tag} {}", self.id)
            },
        )
    }

    fn matches_target(&self, target: &str) -> bool {
        self.ref_name.as_deref() == Some(target) || self.id == target
    }
}

#[derive(Default)]
struct FakeStore {
    images: Vec<FakeImage>,
    present_digests: BTreeSet<String>,
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

    fn by_target(&self, target: &str) -> Option<&FakeImage> {
        self.images
            .iter()
            .find(|image| image.matches_target(target))
    }
}

impl Store for FakeStore {
    fn digest_present(&mut self, _runtime: Runtime, digest: &str) -> Result<bool, image::Error> {
        Ok(self.present_digests.contains(digest))
    }

    fn tag(&mut self, _runtime: Runtime, _source: &str, _target: &str) -> Result<(), image::Error> {
        Ok(())
    }

    fn linux_store_ref(&mut self, image_ref: &str) -> Result<String, image::Error> {
        Ok(format!("containers-storage:{image_ref}"))
    }

    fn copy_oci_layout(
        &mut self,
        source: &OciSource,
        _destination: &str,
    ) -> Result<(), image::Error> {
        self.present_digests.insert(source.digest.clone());
        Ok(())
    }

    fn copy_docker_archive(
        &mut self,
        _archive: &str,
        _destination: &str,
    ) -> Result<(), image::Error> {
        Ok(())
    }

    fn load_docker_archive(&mut self, _archive: &str) -> Result<Option<String>, image::Error> {
        Ok(None)
    }

    fn docker_archive_config_digest(
        &mut self,
        _archive: &str,
    ) -> Result<Option<String>, image::Error> {
        Ok(None)
    }

    fn image_rows(&mut self, _runtime: Runtime) -> Result<Vec<String>, image::Error> {
        Ok(self.images.iter().map(FakeImage::row).collect())
    }

    fn image_id(
        &mut self,
        _runtime: Runtime,
        target: &str,
    ) -> Result<Option<String>, image::Error> {
        Ok(self.by_target(target).map(|image| image.id.clone()))
    }

    fn image_digest(
        &mut self,
        _runtime: Runtime,
        target: &str,
    ) -> Result<Option<String>, image::Error> {
        Ok(self
            .by_target(target)
            .and_then(|image| image.digest.clone()))
    }

    fn image_managed(&mut self, _runtime: Runtime, target: &str) -> Result<bool, image::Error> {
        Ok(self.by_target(target).is_some_and(|image| image.managed))
    }

    fn image_in_use(&mut self, _runtime: Runtime, target: &str) -> Result<bool, image::Error> {
        Ok(self.by_target(target).is_some_and(|image| image.in_use))
    }

    fn delete_image(&mut self, _runtime: Runtime, target: &str) -> Result<(), image::Error> {
        self.deleted.push(target.to_owned());
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

fn digest(ch: char) -> String {
    format!("sha256:{}", ch.to_string().repeat(64))
}
