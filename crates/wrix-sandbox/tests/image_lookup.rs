#![cfg(unix)]

use std::{env, fmt::Write as _, fs, os::unix::fs::PermissionsExt, process::Command};

use serde_json::{Value, json};
use wrix_sandbox::image::{
    self, CommandStore, Digest, ImageId, ImageRef, InstallRequest, Runtime, Source, SourceKind,
    Store, Target,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn apple_native_id_is_resolved_to_an_actual_reference_and_rechecked() -> TestResult {
    let desired = digest('a');
    let fixture = Fixture::new(&[(
        "wrix-selected:test",
        json!([{
            "id": "a".repeat(64),
            "configuration": {"descriptor": {"digest": desired}}
        }]),
    )])?;
    assert_eq!(
        fixture.run_selected(&desired, false, Some(&desired))?,
        Some("a".repeat(64))
    );
    assert_eq!(
        fixture.calls()?,
        [
            "image list --format json",
            "image inspect wrix-selected:test",
            "image inspect wrix-selected:test"
        ]
    );
    Ok(())
}

#[test]
fn apple_digest_lookup_finds_named_image_after_untagged_image() -> TestResult {
    let untagged = format!("untagged@{}", digest('a'));
    let desired = digest('c');
    let fixture = Fixture::new(&[
        (
            &untagged,
            json!([{"id": "untagged-index-id", "digest": digest('b')}]),
        ),
        (
            "wrix-selected:test",
            json!([{"id": "named-index-id", "digest": desired}]),
        ),
    ])?;

    assert_eq!(
        fixture.run(&desired, false)?,
        Some("wrix-selected:test".to_owned())
    );
    assert_eq!(
        fixture.calls()?,
        [
            "image list --format json".to_owned(),
            format!("image inspect {untagged}"),
            "image inspect wrix-selected:test".to_owned(),
        ]
    );
    Ok(())
}

#[test]
fn apple_digest_lookup_returns_none_when_requested_image_is_missing() -> TestResult {
    let untagged = format!("untagged@{}", digest('a'));
    let fixture = Fixture::new(&[
        (
            &untagged,
            json!([{"id": "untagged-index-id", "digest": digest('b')}]),
        ),
        (
            "wrix-selected:test",
            json!([{"id": "named-index-id", "digest": digest('c')}]),
        ),
    ])?;

    assert_eq!(fixture.run(&digest('d'), false)?, None);
    assert_eq!(
        fixture.calls()?,
        [
            "image list --format json".to_owned(),
            format!("image inspect {untagged}"),
            "image inspect wrix-selected:test".to_owned(),
        ]
    );
    Ok(())
}

#[test]
fn apple_cached_named_image_skips_installation_and_tagging() -> TestResult {
    let desired = digest('c');
    let untagged = format!("untagged@{}", digest('a'));
    let fixture = Fixture::new(&[
        (
            "wrix-selected:test",
            json!([{"id": "named-index-id", "digest": desired}]),
        ),
        (
            &untagged,
            json!([{"id": "untagged-index-id", "digest": digest('b')}]),
        ),
    ])?;

    fixture.run(&desired, true)?;
    assert_eq!(
        fixture.calls()?,
        [
            "image list --format json",
            "image inspect wrix-selected:test",
        ]
    );
    Ok(())
}

#[test]
fn apple_cached_untagged_image_is_reused_by_its_full_reference() -> TestResult {
    let untagged = format!("untagged@{}", digest('a'));
    let desired = digest('b');
    let fixture = Fixture::new(&[(
        &untagged,
        json!([{"id": "untagged-index-id", "digest": desired}]),
    )])?;

    assert_eq!(fixture.run(&desired, false)?, Some(untagged.clone()));
    fs::write(fixture.root.path().join("calls"), "")?;
    fixture.run(&desired, true)?;
    assert_eq!(
        fixture.calls()?,
        [
            "image list --format json".to_owned(),
            format!("image inspect {untagged}"),
            format!("image tag {untagged} wrix-selected:test"),
        ]
    );
    Ok(())
}

#[test]
fn apple_digest_lookup_verifies_content_instead_of_reference_or_listing_id() -> TestResult {
    let desired = digest('e');
    let untagged = format!("untagged@{desired}");
    let fixture = Fixture::new(&[(&untagged, json!([{"id": desired, "digest": digest('b')}]))])?;

    assert_eq!(fixture.run(&desired, false)?, None);
    assert_eq!(
        fixture.calls()?,
        [
            "image list --format json".to_owned(),
            format!("image inspect {untagged}"),
        ]
    );
    Ok(())
}

#[test]
fn apple_digest_lookup_preserves_inspection_digest_variants() -> TestResult {
    let desired = digest('a');
    for inspection in [
        json!([{"digest": desired}]),
        json!([{"digest": "a".repeat(64)}]),
        json!([{"id": desired}]),
        json!([{"id": "a".repeat(64)}]),
    ] {
        let fixture = Fixture::new(&[("wrix-selected:test", inspection)])?;
        assert_eq!(
            fixture.run(&desired, false)?,
            Some("wrix-selected:test".to_owned())
        );
    }
    let fixture = Fixture::new(&[("wrix-selected:test", json!([{"id": "opaque-id"}]))])?;
    assert_eq!(fixture.run(&desired, false)?, None);
    Ok(())
}

#[test]
fn image_lookup_container_fixture_conforms_to_json_and_inspection_contract() -> TestResult {
    let untagged = format!("untagged@{}", digest('a'));
    let inspection = json!([{"id": "untagged-index-id", "digest": digest('b')}]);
    let fixture = Fixture::new(&[(&untagged, inspection.clone())])?;
    let run = |args: &[&str]| {
        Command::new(fixture.root.path().join("bin/container"))
            .args(args)
            .env("WRIX_TEST_IMAGE_LOOKUP_STATE", fixture.root.path())
            .output()
    };
    let output = run(&["image", "list", "--format", "json"])?;
    assert!(output.status.success());
    let listed: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(listed[0]["configuration"]["name"], untagged);
    let output = run(&["image", "list"])?;
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)?.contains("untagged <none>"));
    let output = run(&["image", "inspect", &untagged])?;
    assert!(output.status.success());
    assert_eq!(serde_json::from_slice::<Value>(&output.stdout)?, inspection);
    assert_eq!(
        run(&["image", "inspect", "missing:test"])?.status.code(),
        Some(1)
    );
    assert_eq!(run(&["image", "load"])?.status.code(), Some(91));
    Ok(())
}

#[test]
#[ignore = "runs CommandStore in a subprocess with an isolated PATH"]
fn image_lookup_child() -> TestResult {
    let root = std::path::PathBuf::from(
        env::var_os("WRIX_TEST_IMAGE_LOOKUP_STATE").ok_or("missing state")?,
    );
    let desired = Digest::parse(&env::var("WRIX_TEST_IMAGE_LOOKUP_DIGEST")?)?;
    let mut store = CommandStore;
    let result = if let Some(id) = env::var_os("WRIX_TEST_IMAGE_LOOKUP_NATIVE_ID") {
        store
            .image_id(
                Runtime::Container,
                &Target::Id(ImageId::parse(id.to_str().ok_or("invalid native id")?)?),
            )?
            .map(|id| id.as_str().to_owned())
    } else if env::var_os("WRIX_TEST_IMAGE_LOOKUP_INSTALL").is_some() {
        image::install(
            &mut store,
            &InstallRequest::new(
                Runtime::Container,
                &ImageRef::parse("wrix-selected:test")?,
                &Source::parse(root.join("missing-image.tar"), SourceKind::DockerArchive)?,
                Some(&desired),
            )?,
        )?;
        None
    } else {
        store
            .image_for_digest(Runtime::Container, &desired)?
            .map(|target| target.as_str().to_owned())
    };
    fs::write(root.join("result.json"), serde_json::to_vec(&result)?)?;
    Ok(())
}

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new(images: &[(&str, Value)]) -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("bin"))?;
        fs::create_dir(root.path().join("inspect"))?;
        let program = root.path().join("bin/container");
        fs::write(&program, include_str!("fixtures/image-lookup-container.sh"))?;
        fs::set_permissions(program, fs::Permissions::from_mode(0o755))?;
        let mut listed = Vec::new();
        let mut table = String::from("REPOSITORY TAG ID\n");
        for (name, inspection) in images {
            let (repository, tag) = if name.starts_with("untagged@") {
                ("untagged", "<none>")
            } else {
                name.rsplit_once(':').ok_or("missing fixture tag")?
            };
            writeln!(table, "{repository} {tag} {}", digest('e'))?;
            listed.push(json!({"configuration": {"name": name}, "id": digest('e')}));
            fs::write(
                root.path().join("inspect").join(format!("{name}.json")),
                serde_json::to_vec(inspection)?,
            )?;
        }
        fs::write(root.path().join("list.json"), serde_json::to_vec(&listed)?)?;
        fs::write(root.path().join("list.txt"), table)?;
        Ok(Self { root })
    }

    fn run(&self, desired: &str, install: bool) -> TestResult<Option<String>> {
        self.run_selected(desired, install, None)
    }

    fn run_selected(
        &self,
        desired: &str,
        install: bool,
        native_id: Option<&str>,
    ) -> TestResult<Option<String>> {
        let path = env::join_paths(
            std::iter::once(self.root.path().join("bin"))
                .chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
        )?;
        let mut command = Command::new(env::current_exe()?);
        command
            .args(["image_lookup_child", "--exact", "--ignored"])
            .env("PATH", path)
            .env("WRIX_TEST_IMAGE_LOOKUP_STATE", self.root.path())
            .env("WRIX_TEST_IMAGE_LOOKUP_DIGEST", desired)
            .env_remove("WRIX_TEST_IMAGE_LOOKUP_INSTALL")
            .env_remove("WRIX_TEST_IMAGE_LOOKUP_NATIVE_ID");
        if let Some(id) = native_id {
            command.env("WRIX_TEST_IMAGE_LOOKUP_NATIVE_ID", id);
        }
        if install {
            command.env("WRIX_TEST_IMAGE_LOOKUP_INSTALL", "1");
        }
        let output = command.output()?;
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!self.root.path().join("missing-image.tar").exists());
        Ok(serde_json::from_slice(&fs::read(
            self.root.path().join("result.json"),
        )?)?)
    }

    fn calls(&self) -> TestResult<Vec<String>> {
        Ok(fs::read_to_string(self.root.path().join("calls"))?
            .lines()
            .map(str::to_owned)
            .collect())
    }
}

fn digest(ch: char) -> String {
    format!("sha256:{}", ch.to_string().repeat(64))
}
