use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    sync::mpsc,
    thread,
    time::Duration,
};

#[test]
fn realization_streams_diagnostics_before_the_builder_exits() {
    let root = tempfile::tempdir().unwrap();
    let nix = root.path().join("nix-store");
    fs::write(
        &nix,
        "#!/bin/bash\nset -euo pipefail\nprintf 'live build diagnostic\\n' >&2\nread -r release\nexit 23\n",
    )
    .unwrap();
    fs::set_permissions(&nix, fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "lifecycle::binding::test::realization_child",
            "--ignored",
            "--nocapture",
        ])
        .env("WRIX_TEST_REALIZATION_ROOT", root.path())
        .env("PATH", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if line.unwrap() == "live build diagnostic" {
                sender.send(()).unwrap();
                break;
            }
        }
    });
    let streamed = receiver.recv_timeout(Duration::from_secs(5));
    let was_running = child.try_wait().unwrap().is_none();
    child.stdin.take().unwrap().write_all(b"release\n").unwrap();
    let status = child.wait().unwrap();
    reader.join().unwrap();
    assert!(streamed.is_ok(), "Nix diagnostics were buffered");
    assert!(was_running, "the diagnostic arrived only after exit");
    assert!(status.success(), "realization child failed: {status}");
}

#[test]
#[ignore = "subprocess helper for isolated PATH and inherited stderr"]
fn realization_child() {
    let root = PathBuf::from(env::var_os("WRIX_TEST_REALIZATION_ROOT").unwrap());
    let artifact = Artifact {
        path: root.join("source"),
        derivation: root.join("source.drv"),
    };
    let error = artifact
        .realize(&root.join("roots"), &artifact.path)
        .unwrap_err();
    assert!(error.to_string().contains("23"), "{error}");
    assert!(error.to_string().contains("source.drv"), "{error}");
}

fn resource_binding() -> serde_json::Value {
    serde_json::json!({
        "schema": 2,
        "image": "wrix-service:resource-test",
        "source_kind": "docker-archive",
        "source": {
            "path": "/nix/store/test-service/image.tar.gz",
            "derivation": "/nix/store/test-service.drv"
        },
        "digest": {
            "path": "/nix/store/test-digest/digest",
            "derivation": "/nix/store/test-digest.drv"
        },
        "resources": {"cpus":8,"memory_mib":8192}
    })
}

#[test]
fn resource_binding_survives_recovery_serialization() {
    let wire = resource_binding();
    let candidate: Candidate = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(candidate).unwrap(), wire);
}

#[test]
fn retained_legacy_binding_does_not_acquire_new_limits() {
    let mut wire = resource_binding();
    wire["schema"] = 1.into();
    wire.as_object_mut().unwrap().remove("resources");
    let candidate: Candidate = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(candidate).unwrap(), wire);
}

#[test]
fn resource_schema_requires_limits() {
    let mut wire = resource_binding();
    wire.as_object_mut().unwrap().remove("resources");
    assert!(serde_json::from_value::<Candidate>(wire).is_err());
}

#[test]
fn legacy_schema_cannot_smuggle_resource_limits() {
    let mut wire = resource_binding();
    wire["schema"] = 1.into();
    assert!(serde_json::from_value::<Candidate>(wire).is_err());
}

#[test]
fn resource_change_is_an_explicit_package_change() {
    let wire = resource_binding();
    let old: Candidate = serde_json::from_value(wire.clone()).unwrap();
    let mut changed = wire;
    changed["resources"]["cpus"] = 4.into();
    let new: Candidate = serde_json::from_value(changed).unwrap();
    assert_ne!(old, new);
}
