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
