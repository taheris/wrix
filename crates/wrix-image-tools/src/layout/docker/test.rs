use super::*;
use crate::layout::test::Fixture;
use std::{
    process::Command,
    time::{Duration, Instant},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn extract(archive: &Path, member: &str) -> Vec<u8> {
    let result = Command::new("tar")
        .arg("-xOf")
        .arg(archive)
        .arg(member)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

#[test]
fn cached_entries_and_final_tail_reconstruct_the_exact_archive_across_padding_boundaries()
-> TestResult {
    for size in [0, 1, 511, 512, 513, 10239] {
        let fixture = Fixture::new();
        let contents = fixture.contents("package");
        fs::write(contents.join("variable"), vec![b'x'; size])?;
        let conf = fixture.conf("base", &contents, &["BASE=1"]);
        let layout = fixture.build(&conf, "layout", &[])?;
        let mut expected = Vec::new();
        stream(&layout, &conf, None, None, &mut expected)?;
        let mut actual = Vec::new();
        let manifest = Fixture::manifest(&layout);
        for layer in manifest["layers"].as_array().unwrap() {
            let digest = Digest::prefixed(layer["digest"].as_str().unwrap())?;
            layer_entry(&layout, &conf, &digest.0, &mut actual)?;
        }
        tail(&layout, &conf, None, &mut actual)?;
        assert_eq!(actual, expected, "size={size}");
    }
    Ok(())
}

#[test]
fn archive_export_preserves_cached_layer_bytes_checksums_and_reproducibility() -> TestResult {
    let fixture = Fixture::new();
    let contents = fixture.contents("package");
    fs::hard_link(contents.join("file"), contents.join("hardlink"))?;
    fs::write(contents.join("empty"), b"")?;
    let conf = fixture.conf("base", &contents, &["BASE=1"]);
    let layout = fixture.build(&conf, "layout", &[])?;
    let mut first = Vec::new();
    stream(&layout, &conf, None, None, &mut first)?;
    let mut second = Vec::new();
    stream(&layout, &conf, None, None, &mut second)?;
    assert_eq!(first, second);
    let archive = fixture.path("export.tar");
    fs::write(&archive, first)?;
    let manifest: Value = serde_json::from_slice(&extract(&archive, "manifest.json"))?;
    assert_eq!(manifest[0]["RepoTags"], json!(["test:latest"]));
    let source = Fixture::manifest(&layout);
    for (member, descriptor) in manifest[0]["Layers"]
        .as_array()
        .unwrap()
        .iter()
        .zip(source["layers"].as_array().unwrap())
    {
        let digest = Digest::prefixed(descriptor["digest"].as_str().unwrap())?;
        let bytes = extract(&archive, member.as_str().unwrap());
        assert_eq!(
            bytes,
            fs::read(layout.join("blobs/sha256").join(&digest.0))?
        );
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), digest.0);
        assert_eq!(
            u64::try_from(bytes.len())?,
            descriptor["size"].as_u64().unwrap()
        );
    }
    let name = manifest[0]["Config"].as_str().unwrap();
    let bytes = extract(&archive, name);
    assert_eq!(format!("{:x}.json", Sha256::digest(&bytes)), name);
    let config: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(
        config["rootfs"]["diff_ids"],
        Value::Array(
            source["layers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|layer| layer["digest"].clone())
                .collect()
        )
    );
    Ok(())
}

#[test]
fn inherited_history_and_explicit_repository_tag_match_the_stream_contract() -> TestResult {
    let fixture = Fixture::new();
    let base = fixture.conf(
        "base",
        &fixture.contents("base-package"),
        &["BASE=1", "SHARED=base"],
    );
    let parent = fixture.build(&base, "parent", &[])?;
    let leaf = fixture.conf("leaf", &fixture.contents("leaf-package"), &["SHARED=leaf"]);
    let layout = fixture.build(
        &leaf,
        "leaf",
        &["--from-layout".as_ref(), parent.as_os_str()],
    )?;
    let mut bytes = Vec::new();
    stream(
        &layout,
        &leaf,
        Some(&parent),
        Some("alternate:tag"),
        &mut bytes,
    )?;
    let archive = fixture.path("export.tar");
    fs::write(&archive, bytes)?;
    let manifest: Value = serde_json::from_slice(&extract(&archive, "manifest.json"))?;
    assert_eq!(manifest[0]["RepoTags"], json!(["alternate:tag"]));
    let config: Value =
        serde_json::from_slice(&extract(&archive, manifest[0]["Config"].as_str().unwrap()))?;
    assert_eq!(config["config"]["Env"], json!(["BASE=1", "SHARED=leaf"]));
    assert_eq!(config["history"][0]["comment"], "store paths: None");
    assert_eq!(config["history"][1]["comment"], "store paths: None");
    assert!(
        config["history"][2]["comment"]
            .as_str()
            .unwrap()
            .contains("leaf-package")
    );
    Ok(())
}

#[test]
fn early_and_late_missing_producers_are_nonzero_without_waiting_for_a_thread() -> TestResult {
    for late in [false, true] {
        let fixture = Fixture::new();
        let package = fixture.contents("package");
        fs::write(package.join("payload"), vec![b'x'; 4 * 1024 * 1024])?;
        let conf = fixture.conf("base", &package, &[]);
        let layout = fixture.build(&conf, "layout", &[])?;
        let manifest = Fixture::manifest(&layout);
        let index = usize::from(late);
        let digest = Digest::prefixed(manifest["layers"][index]["digest"].as_str().unwrap())?;
        fs::remove_file(layout.join("blobs/sha256").join(digest.0))?;
        let mut output = Vec::new();
        let started = Instant::now();
        assert!(stream(&layout, &conf, None, None, &mut output).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        if late {
            assert!(output.len() > 4 * 1024 * 1024);
        } else {
            assert_eq!(output, [] as [u8; 0]);
        }
    }
    Ok(())
}

struct Consumer {
    remaining: usize,
    bytes: Vec<u8>,
    fail_flush: bool,
}
impl Write for Consumer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::other("injected consumer failure"));
        }
        let written = bytes.len().min(self.remaining);
        self.remaining -= written;
        self.bytes.extend_from_slice(&bytes[..written]);
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::Error::other("injected late completion failure"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn early_consumer_and_late_completion_failures_never_become_success() -> TestResult {
    let fixture = Fixture::new();
    let package = fixture.contents("package");
    fs::write(package.join("payload"), vec![b'x'; 4 * 1024 * 1024])?;
    let conf = fixture.conf("base", &package, &[]);
    let layout = fixture.build(&conf, "layout", &[])?;
    for (remaining, fail_flush) in [(4096, false), (usize::MAX, true)] {
        let mut consumer = Consumer {
            remaining,
            bytes: Vec::new(),
            fail_flush,
        };
        let started = Instant::now();
        let error = stream(&layout, &conf, None, None, &mut consumer).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3));
        if fail_flush {
            assert!(
                error
                    .to_string()
                    .contains("injected late completion failure")
            );
            assert!(consumer.bytes.len() > 4 * 1024 * 1024);
        } else {
            assert!(error.to_string().contains("injected consumer failure"));
            assert_eq!(consumer.bytes.len(), 4096);
        }
    }
    Ok(())
}
