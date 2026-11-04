//! Opt-in native primitive acceptance: no service state, host mounts or network.
use super::super::{Runtime, RuntimeKind, RuntimeStatus};
use std::{
    fs,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use wrix_core::path::ContainerName;
use wrix_sandbox::image::Digest;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
#[ignore = "requires explicit WRIX_NATIVE_APPLE_IDENTITY_IMAGE and native Apple runtime"]
fn native_stopped_apple_identity_refusal_precedes_start_and_owned_cleanup() -> TestResult {
    let image = std::env::var("WRIX_NATIVE_APPLE_IDENTITY_IMAGE")?;
    let metadata = Command::new("container")
        .args(["image", "inspect", &image])
        .stdin(Stdio::null())
        .output()?;
    assert!(
        metadata.status.success(),
        "{}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    assert_eq!(metadata.as_array().map(Vec::len), Some(1));
    let expected = Digest::parse(
        metadata[0]["configuration"]["descriptor"]["digest"]
            .as_str()
            .ok_or("missing native index")?,
    )?;
    let temporary = tempfile::Builder::new()
        .prefix("wrix-identity-")
        .tempdir_in(fs::canonicalize("/tmp")?)?;
    let marker = temporary.path().file_name().unwrap().to_str().unwrap();
    let name = ContainerName::from_persisted(marker.to_owned()).ok_or("invalid fixture name")?;
    let labels = [
        ("wrix.kind", "identity-fixture"),
        ("wrix.identity.fixture", marker),
    ];
    let runtime = Runtime {
        binary: "container".into(),
        kind: RuntimeKind::Container,
        binding: None,
    };
    let created = Command::new("container")
        .args([
            "create",
            "--name",
            name.as_str(),
            "--network",
            "none",
            "--entrypoint",
            "/bin/true",
            "--label",
            "wrix.kind=identity-fixture",
            "--label",
        ])
        .arg(format!("wrix.identity.fixture={marker}"))
        .arg(&image)
        .stdin(Stdio::null())
        .output()?;
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let primary = std::panic::catch_unwind(|| -> TestResult {
        let before = runtime
            .apple_snapshot(name.as_str())?
            .ok_or("created fixture is missing")?;
        assert_eq!(before.runtime_status(), RuntimeStatus::Stopped);
        assert_eq!(before.image_id(), Some(&expected));
        let wrong = Digest::parse(&format!("sha256:{}", "b".repeat(64)))?;
        assert_ne!(wrong, expected);
        assert!(runtime.start_bound_apple(&name, &wrong, &labels).is_err());
        assert_eq!(
            runtime
                .apple_snapshot(name.as_str())?
                .ok_or("fixture disappeared")?
                .runtime_status(),
            RuntimeStatus::Stopped
        );
        runtime.start_bound_apple(&name, &expected, &labels)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let snapshot = runtime
                .apple_snapshot(name.as_str())?
                .ok_or("started fixture disappeared")?;
            if snapshot.runtime_status() == RuntimeStatus::Stopped {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned trivial fixture did not exit"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    });
    let cleanup = std::panic::catch_unwind(|| -> TestResult {
        let snapshot = runtime
            .apple_snapshot(name.as_str())?
            .ok_or("cannot prove fixture cleanup identity")?;
        if snapshot.image_id() != Some(&expected)
            || labels.iter().any(|(key, value)| {
                snapshot.configuration.labels.get(*key).map(String::as_str) != Some(*value)
            })
        {
            return Err("fixture identity changed; retaining it without stop/removal".into());
        }
        if snapshot.runtime_status() == RuntimeStatus::Running {
            let stop = Command::new("container")
                .args(["stop", "--time", "30", name.as_str()])
                .output()?;
            assert!(
                stop.status.success(),
                "{}",
                String::from_utf8_lossy(&stop.stderr)
            );
        }
        assert_eq!(
            runtime
                .apple_snapshot(name.as_str())?
                .ok_or("cannot observe stopped fixture")?
                .runtime_status(),
            RuntimeStatus::Stopped
        );
        let removed = Command::new("container")
            .args(["rm", name.as_str()])
            .output()?;
        assert!(
            removed.status.success(),
            "{}",
            String::from_utf8_lossy(&removed.stderr)
        );
        assert!(runtime.apple_snapshot(name.as_str())?.is_none());
        Ok(())
    });
    if let Ok(Err(error)) = &primary {
        eprintln!(
            "native identity primary failure for {}: {error}",
            name.as_str()
        );
    }
    if let Ok(Err(error)) = &cleanup {
        eprintln!(
            "native identity cleanup failure for {}: {error}",
            name.as_str()
        );
    }
    match primary {
        Ok(result) => result?,
        Err(panic) => std::panic::resume_unwind(panic),
    }
    match cleanup {
        Ok(result) => result?,
        Err(panic) => std::panic::resume_unwind(panic),
    }
    eprintln!(
        "native stopped/create/start identity and verified owned removal: {} ({})",
        name.as_str(),
        expected.as_str()
    );
    Ok(())
}
