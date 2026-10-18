#[path = "common/lifecycle/mod.rs"]
mod lifecycle;

use std::{collections::BTreeSet, fs, path::Path};

use serde_json::json;
use wrix_sandbox::image::{
    self, Digest, ImageId, ImageRef, ImageRow, InstallRequest, OciSource, Runtime, Source,
    SourceKind, Store, Target,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn launch_installation_does_not_prune_cached_images() -> TestResult {
    for mode in ["run", "spawn"] {
        let fixture = lifecycle::Fixture::new()?;
        let cached = (0..12)
            .map(|index| {
                json!({
                    "ref": format!("localhost/wrix-cached:{index}"),
                    "id": format!("id-{index}")
                })
            })
            .collect::<Vec<_>>();
        let mru = serde_json::to_vec(&cached)?;
        fs::write(fixture.root().join("mru.json"), &mru)?;
        let mut child = std::process::Command::new(std::env::current_exe()?);
        child.args(["launch_image_child", "--exact", "--ignored"]);
        if mode == "spawn" {
            fixture.assert_foreground(child, false, 0)?;
        } else {
            fixture.launched_argv(child, &[])?;
        }
        assert_eq!(fs::read(fixture.root().join("mru.json"))?, mru);
        let calls = fs::read_to_string(fixture.root().join("argv.calls"))?;
        assert!(!calls.lines().any(|line| line.starts_with("images ")
            || line.starts_with("rmi ")
            || line.starts_with("ps -a")));
        assert!(!calls.contains("image delete"));
        assert!(!calls.contains("image load"));
        assert!(!fixture.root().join("cache/wrix/image-mru.json").exists());
    }
    Ok(())
}

#[test]
#[ignore = "child process dispatches the production launcher with an isolated runtime"]
fn launch_image_child() -> TestResult {
    let profile = std::env::var_os("WRIX_TEST_PROFILE_CONFIG").ok_or("profile missing")?;
    let (mode, args) = if std::env::var_os("WRIX_TEST_STDIO").is_some() {
        (
            wrix_sandbox::command::Command::Spawn,
            vec![
                "--spawn-config".to_owned(),
                std::env::var("WRIX_TEST_SPAWN_CONFIG")?,
            ],
        )
    } else {
        (
            wrix_sandbox::command::Command::Run,
            vec![std::env::var("WRIX_TEST_WORKSPACE")?],
        )
    };
    assert_eq!(
        wrix_sandbox::command::run(
            mode,
            Some(profile.into()),
            &args,
            &mut std::io::stdout(),
            &mut std::io::stderr()
        )?,
        std::process::ExitCode::SUCCESS
    );
    Ok(())
}

#[test]
fn installation_preserves_more_than_eight_images_on_hits_and_misses() -> TestResult {
    for runtime in [Runtime::Podman, Runtime::Container] {
        for installed in [false, true] {
            let root = tempfile::tempdir()?;
            let source_path = root.path().join("source");
            let kind = match runtime {
                Runtime::Podman => SourceKind::NixDescriptor,
                Runtime::Container => SourceKind::DockerArchive,
            };
            let source = Source::parse(&source_path, kind)?;
            let desired = digest('c');
            if !installed {
                fs::write(
                    &source_path,
                    serde_json::to_vec(&json!({
                        "digest": desired, "oci_layout": "/fixture/layout", "oci_ref": "latest"
                    }))?,
                )?;
            }
            let mut store = FakeStore::with_images(
                (0..12)
                    .map(|index| {
                        fake_image(
                            &format!("localhost/wrix-cached:{index}"),
                            &format!("id-{index}"),
                        )
                        .managed()
                    })
                    .collect(),
            );
            if installed {
                store.present_digests.insert(desired.clone());
            }
            image::install(
                &mut store,
                &InstallRequest::new(
                    runtime,
                    &ImageRef::parse("wrix-current:latest")?,
                    &source,
                    Some(&desired),
                )?,
            )?;
            assert_eq!(store.images.len(), 12);
            assert_eq!(store.deleted, Vec::<String>::new());
            assert_eq!(store.copies, usize::from(!installed));
            assert_eq!(store.listings, 0);
            assert_eq!(source_path.exists(), !installed);
        }
    }
    Ok(())
}

#[test]
fn image_inventory_is_read_only() -> TestResult {
    for runtime in [Runtime::Podman, Runtime::Container] {
        let mut store = FakeStore::with_images(vec![
            fake_image("wrix-current:latest", "idle-id").managed(),
            fake_image("foreign:latest", "foreign-id").in_use(),
            FakeImage::dangling("dangling-id"),
        ]);
        let result = image::inventory(&mut store, runtime)?;
        assert_eq!(result.len(), 3);
        assert!(result[0].managed);
        assert!(!result[0].in_use);
        assert!(!result[1].managed);
        assert!(result[1].in_use);
        assert_eq!(result[2].id, ImageId::parse("dangling-id")?);
        assert_eq!(store.deleted, Vec::<String>::new());
        assert_eq!(store.copies, 0);
        assert_eq!(store.tags, 0);
        assert!(store.present_digests.is_empty());
    }
    Ok(())
}

#[test]
fn inspection_checks_usage_by_identity_not_current_tag_or_name() -> TestResult {
    for runtime in [Runtime::Podman, Runtime::Container] {
        let mut store = FakeStore::with_images(vec![
            fake_image("wrix-current:latest", "used-id")
                .managed()
                .in_use(),
            fake_image("localhost/wrix-user:old", "unowned-id"),
            fake_image("renamed:latest", "idle-id").managed(),
        ]);
        let rows = image::inventory(&mut store, runtime)?;
        assert!(rows[0].in_use);
        assert!(!rows[1].managed);
        assert!(!rows[2].in_use);
        assert_eq!(
            store.usage_targets,
            vec![
                Target::Id(ImageId::parse("used-id")?),
                Target::Id(ImageId::parse("unowned-id")?),
                Target::Id(ImageId::parse("idle-id")?),
            ]
        );
    }
    Ok(())
}

#[test]
fn inspection_rejects_missing_current_identity() -> TestResult {
    let mut store = FakeStore::default();
    assert!(matches!(
        image::inspect(
            &mut store,
            Runtime::Podman,
            &Target::Reference(ImageRef::parse("missing:latest")?)
        ),
        Err(image::Error::MissingImageId { .. })
    ));
    assert_eq!(store.deleted, Vec::<String>::new());
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
    copies: usize,
    tags: usize,
    listings: usize,
    usage_targets: Vec<Target>,
}

impl FakeStore {
    const fn with_images(images: Vec<FakeImage>) -> Self {
        Self {
            images,
            present_digests: BTreeSet::new(),
            deleted: Vec::new(),
            copies: 0,
            tags: 0,
            listings: 0,
            usage_targets: Vec::new(),
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
        self.tags += 1;
        Ok(())
    }

    fn copy_oci_layout(
        &mut self,
        source: &OciSource,
        _destination: &ImageRef,
    ) -> Result<(), image::Error> {
        self.copies += 1;
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
    fn load_docker_archive(&mut self, archive: &Path) -> Result<Option<ImageRef>, image::Error> {
        assert!(archive.is_file());
        self.copies += 1;
        Ok(None)
    }
    fn docker_archive_config_digest(
        &mut self,
        _archive: &Path,
    ) -> Result<Option<Digest>, image::Error> {
        Ok(None)
    }

    fn image_rows(&mut self, _runtime: Runtime) -> Result<Vec<ImageRow>, image::Error> {
        self.listings += 1;
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
        self.usage_targets.push(target.clone());
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
