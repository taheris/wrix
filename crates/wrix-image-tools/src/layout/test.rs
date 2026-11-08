use super::*;
use std::{
    os::unix::fs::{PermissionsExt, symlink},
    process::Command,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
pub(super) struct Fixture {
    root: tempfile::TempDir,
}
impl Fixture {
    pub(super) fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }
    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
    pub(super) fn contents(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir(&path).unwrap();
        fs::write(path.join("file"), name).unwrap();
        symlink("file", path.join("link")).unwrap();
        path
    }
    pub(super) fn conf(&self, name: &str, contents: &Path, env: &[&str]) -> PathBuf {
        let custom = self.path(&format!("{name}-custom"));
        fs::create_dir(&custom).unwrap();
        let status = Command::new("tar")
            .args(["-cf"])
            .arg(custom.join("layer.tar"))
            .arg("-C")
            .arg(contents)
            .arg(format!("--transform=s,^\\.,{name},"))
            .arg(".")
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(
            custom.join("checksum"),
            format!(
                "{:x}",
                Sha256::digest(fs::read(custom.join("layer.tar")).unwrap())
            ),
        )
        .unwrap();
        let path = self.path(&format!("{name}.json"));
        let conf = json!({"created":"1970-01-01T00:00:01+00:00", "mtime":"1970-01-01T00:00:01+00:00", "uid":"0", "gid":"0", "uname":"root", "gname":"root", "architecture":"amd64", "os":"linux", "repo_tag":"test:latest", "from_image": null, "store_layers":[[contents]], "customisation_layer":custom, "config":{"Env":env}});
        fs::write(&path, serde_json::to_vec(&conf).unwrap()).unwrap();
        path
    }
    pub(super) fn build(
        &self,
        conf: &Path,
        name: &str,
        args: &[&std::ffi::OsStr],
    ) -> Result<PathBuf, Error> {
        let output = self.path(name);
        let mut values = vec![conf.as_os_str().to_owned(), output.as_os_str().to_owned()];
        values.extend(args.iter().map(|value| (*value).to_owned()));
        build(&Options::parse(&values)?)?;
        Ok(output)
    }
    pub(super) fn manifest(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path.join("wrix/manifest.json")).unwrap()).unwrap()
    }
}

#[test]
fn store_layer_is_reproducible_and_preserves_payload_ownership_symlinks_and_materialized_hardlinks()
-> TestResult {
    let fixture = Fixture::new();
    let contents = fixture.contents("package");
    fs::hard_link(contents.join("file"), contents.join("hardlink"))?;
    fs::set_permissions(contents.join("file"), fs::Permissions::from_mode(0o751))?;
    symlink("nix-relative-target", contents.join("nix-link"))?;
    let conf = fixture.conf("base", &contents, &[]);
    let first = fixture.build(&conf, "first", &["--store-layers-only".as_ref()])?;
    let second = fixture.build(&conf, "second", &["--store-layers-only".as_ref()])?;
    assert_eq!(
        fs::read(first.join("layers.json"))?,
        fs::read(second.join("layers.json"))?
    );
    let layers: Vec<Layer> = serde_json::from_slice(&fs::read(first.join("layers.json"))?)?;
    let blob = first.join("blobs/sha256").join(&layers[0].digest.0);
    let bytes = fs::read(&blob)?;
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), layers[0].digest.0);
    assert_eq!(u64::try_from(bytes.len())?, layers[0].size);
    let mut offset = 0;
    let mut seen = std::collections::BTreeMap::new();
    while bytes[offset..offset + 512].iter().any(|byte| *byte != 0) {
        let header = &bytes[offset..offset + 512];
        let field = |start, end| {
            std::str::from_utf8(&header[start..end])
                .unwrap()
                .trim_matches(['\0', ' '])
        };
        let size = usize::from_str_radix(field(124, 136), 8)?;
        assert_eq!(u64::from_str_radix(field(108, 116), 8)?, 0);
        assert_eq!(u64::from_str_radix(field(116, 124), 8)?, 0);
        assert_eq!(i64::from_str_radix(field(136, 148), 8)?, 1);
        let name = field(0, 100).to_owned();
        seen.insert(
            name,
            (
                header[156],
                field(157, 257).to_owned(),
                header[100..108].to_vec(),
                bytes[offset + 512..offset + 512 + size].to_vec(),
            ),
        );
        offset += 512 + size.div_ceil(512) * 512;
    }
    let member = |name: &str| {
        seen.iter()
            .find(|(path, _)| path.ends_with(&format!("/{name}")))
            .map(|(_, entry)| entry)
            .unwrap()
    };
    assert_eq!(member("link").0, b'2');
    assert_eq!(member("link").1, "file");
    assert_eq!(member("nix-link").1, "nix-relative-target");
    assert_eq!(member("hardlink").0, b'0');
    assert_eq!(member("hardlink").3, b"package");
    assert_eq!(member("file").2, b"0000751\0");
    for name in ["/nix/", "/nix/store/"] {
        assert_eq!(seen[name].2, b"0000755\0");
    }
    Ok(())
}

#[test]
fn child_reuses_parent_and_package_blobs_and_preserves_environment_overlay() -> TestResult {
    let fixture = Fixture::new();
    let base_contents = fixture.contents("base-package");
    let base = fixture.conf("base", &base_contents, &["BASE=1", "SHARED=base"]);
    let parent = fixture.build(&base, "parent", &[])?;
    let leaf_contents = fixture.contents("leaf-package");
    let leaf = fixture.conf("leaf", &leaf_contents, &["SHARED=leaf"]);
    let store = fixture.build(&leaf, "store", &["--store-layers-only".as_ref()])?;
    let args = [
        "--from-layout".as_ref(),
        parent.as_os_str(),
        "--store-layers".as_ref(),
        store.as_os_str(),
    ];
    let child = fixture.build(&leaf, "child", &args)?;
    let manifest = Fixture::manifest(&child);
    let parent_manifest = Fixture::manifest(&parent);
    assert_eq!(
        manifest["layers"].as_array().unwrap()[..2],
        parent_manifest["layers"].as_array().unwrap()[..]
    );
    for layer in manifest["layers"].as_array().unwrap() {
        let digest = Digest::prefixed(layer["digest"].as_str().unwrap())?;
        let blob = child.join("blobs/sha256").join(&digest.0);
        assert!(fs::symlink_metadata(&blob)?.file_type().is_symlink());
        assert_eq!(format!("{:x}", Sha256::digest(fs::read(&blob)?)), digest.0);
        assert_eq!(fs::metadata(blob)?.len(), layer["size"].as_u64().unwrap());
    }
    let digest = Digest::prefixed(manifest["config"]["digest"].as_str().unwrap())?;
    let config: Value =
        serde_json::from_slice(&fs::read(child.join("blobs/sha256").join(digest.0))?)?;
    assert_eq!(config["config"]["Env"], json!(["BASE=1", "SHARED=leaf"]));
    assert_eq!(
        config["rootfs"]["diff_ids"],
        Value::Array(
            manifest["layers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["digest"].clone())
                .collect()
        )
    );
    fs::remove_dir_all(base_contents)?;
    fs::remove_dir_all(leaf_contents)?;
    for layout in [&parent, &store] {
        let layers: Vec<String> = if *layout == parent {
            Fixture::manifest(layout)["layers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    entry["digest"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("sha256:")
                        .unwrap()
                        .to_owned()
                })
                .collect()
        } else {
            serde_json::from_slice::<Vec<Layer>>(&fs::read(layout.join("layers.json"))?)?
                .iter()
                .map(|entry| entry.digest.0.clone())
                .collect()
        };
        for digest in layers {
            let blob = layout.join("blobs/sha256").join(digest);
            fs::remove_file(&blob)?;
            symlink("/nonexistent/cached-layer-read-negative-control", blob)?;
        }
    }
    let unreadable = fixture.build(&leaf, "unreadable-cache-control", &args)?;
    assert_eq!(Fixture::manifest(&unreadable), manifest);
    Ok(())
}

#[test]
fn cached_customisation_payload_is_statted_without_opening_its_bytes() -> TestResult {
    let fixture = Fixture::new();
    let conf = fixture.conf("base", &fixture.contents("package"), &[]);
    let store = fixture.build(&conf, "store", &["--store-layers-only".as_ref()])?;
    let payload = fixture.path("base-custom/layer.tar");
    fs::remove_file(&payload)?;
    fs::create_dir(&payload)?;
    let output = fixture.build(
        &conf,
        "metadata-only-control",
        &["--store-layers".as_ref(), store.as_os_str()],
    )?;
    let manifest = Fixture::manifest(&output);
    let layers = manifest["layers"].as_array().unwrap();
    assert_eq!(layers.last().unwrap()["size"], fs::metadata(payload)?.len());
    Ok(())
}

#[test]
fn archive_parent_is_rejected_before_output_creation() -> TestResult {
    let fixture = Fixture::new();
    let conf = fixture.conf("base", &fixture.contents("package"), &[]);
    let mut value: Value = serde_json::from_slice(&fs::read(&conf)?)?;
    value["from_image"] = json!("/unavailable/whole-image.tar.gz");
    fs::write(&conf, serde_json::to_vec(&value)?)?;
    assert!(matches!(
        fixture.build(&conf, "output", &[]),
        Err(Error::ArchiveParent)
    ));
    assert!(!fixture.path("output").exists());
    Ok(())
}

#[test]
fn generated_change_preserves_shared_package_blob_identity() -> TestResult {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    let contents = fixture.contents("package");
    let first_conf = fixture.conf("first", &contents, &["SETTING=first"]);
    let second_conf = fixture.conf("second", &contents, &["SETTING=second"]);
    let store = fixture.build(&first_conf, "store", &["--store-layers-only".as_ref()])?;
    let args = ["--store-layers".as_ref(), store.as_os_str()];
    let first = fixture.build(&first_conf, "first", &args)?;
    let second = fixture.build(&second_conf, "second", &args)?;
    let a = Fixture::manifest(&first);
    let b = Fixture::manifest(&second);
    assert_eq!(a["layers"][0], b["layers"][0]);
    assert_ne!(a["layers"][1], b["layers"][1]);
    let digest = Digest::prefixed(a["layers"][0]["digest"].as_str().unwrap())?;
    let a = fs::metadata(first.join("blobs/sha256").join(&digest.0))?;
    let b = fs::metadata(second.join("blobs/sha256").join(&digest.0))?;
    assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));
    Ok(())
}

#[test]
fn timestamp_and_history_rendering_preserve_upstream_semantics() -> TestResult {
    assert_eq!(
        created("1970-01-01T00:00:01Z")?,
        "1970-01-01T00:00:01+00:00"
    );
    assert_eq!(
        created("1970-01-01T00:00:01.25+02:00")?,
        "1970-01-01T00:00:01.250000+02:00"
    );
    assert_eq!(created("1970-01-01T00:00:01")?, "1970-01-01T00:00:01");
    assert_eq!(
        timestamp("1969-12-31T23:59:59.9+00:00")?.timestamp_micros() / 1_000_000,
        0
    );
    assert!(timestamp("now").is_ok());
    assert!(timestamp("not a timestamp").is_err());
    assert_eq!(python_repr("/nix/a'b"), "\"/nix/a'b\"");
    assert_eq!(python_repr("/nix/a\nb"), "'/nix/a\\nb'");
    for digest in ["../escape", "SHA256:invalid", "", &"A".repeat(64)] {
        assert!(Digest::try_from(digest.to_owned()).is_err());
    }
    for values in [
        vec![],
        vec!["conf"],
        vec!["conf", "out", "--other"],
        vec!["conf", "out", "--store-layers"],
        vec!["conf", "out", "--store-layers-only", "--store-layers-only"],
    ] {
        assert!(
            Options::parse(&values.into_iter().map(OsString::from).collect::<Vec<_>>()).is_err()
        );
    }
    Ok(())
}
