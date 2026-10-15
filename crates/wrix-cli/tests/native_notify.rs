use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

fn launch(root: &Path, name: &str) -> std::io::Result<std::process::Child> {
    Command::new(env!("CARGO_BIN_EXE_wrix"))
        .args(["notify", "run", "--", "sh", "-c", "printf '%s' \"${WRIX_FOCUS_TARGET:-}\" > \"$OUTPUT\"; while [ ! -f \"$RELEASE\" ]; do sleep 0.02; done; exit 23"])
        .env("XDG_RUNTIME_DIR", root)
        .env("XDG_DATA_HOME", root)
        .env("HOME", root)
        .env("OUTPUT", root.join(name))
        .env("RELEASE", root.join(format!("{name}-release")))
        .env("WRIX_FOCUS_TARGET", "untrusted-inherited-target")
        .env_remove("TMUX").env_remove("TMUX_PANE")
        .stdin(Stdio::null()).spawn()
}

fn sessions(root: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        root.join(".local/share/wrix/sessions")
    } else {
        root.join("wrix/sessions")
    }
}

fn target(root: &Path, name: &str) -> std::io::Result<String> {
    let output = root.join(name);
    for _ in 0..500 {
        if output.exists() {
            let value = fs::read_to_string(&output)?;
            if !value.is_empty() {
                return Ok(value);
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "native command did not publish its target",
    ))
}

#[test]
fn concurrent_launches_at_shared_origin_have_independent_lifetimes() {
    let root = tempfile::tempdir().unwrap();
    let mut first = launch(root.path(), "first").unwrap();
    let mut second = launch(root.path(), "second").unwrap();
    let one = target(root.path(), "first").unwrap();
    let two = target(root.path(), "second").unwrap();
    assert_ne!(one, two);
    let sessions = sessions(root.path());
    for value in [&one, &two] {
        assert!(value.starts_with("native-"));
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(sessions.join(format!("{value}.json"))).unwrap())
                .unwrap();
        assert_eq!(record["leased"], true);
        assert_eq!(record["focus_target"], *value);
    }
    fs::write(root.path().join("first-release"), "").unwrap();
    assert_eq!(first.wait().unwrap().code(), Some(23));
    assert!(!sessions.join(format!("{one}.json")).exists());
    assert!(!sessions.join(format!("{one}.lease")).exists());
    assert!(sessions.join(format!("{two}.json")).exists());
    fs::write(root.path().join("second-release"), "").unwrap();
    assert_eq!(second.wait().unwrap().code(), Some(23));
    assert_eq!(fs::read_dir(sessions).unwrap().count(), 0);
}

#[test]
fn registration_failure_still_runs_command_without_inherited_routing() {
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("blocked");
    fs::write(&blocked, "not a directory").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wrix"))
        .args([
            "notify",
            "run",
            "--",
            "sh",
            "-c",
            "printf 'ran:%s' \"${WRIX_FOCUS_TARGET-unset}\"; exit 17",
        ])
        .env("XDG_RUNTIME_DIR", &blocked)
        .env("XDG_DATA_HOME", &blocked)
        .env("HOME", &blocked)
        .env("WRIX_FOCUS_TARGET", "inherited")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(17));
    assert_eq!(output.stdout, b"ran:unset");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("registration unavailable")
    );
}

#[test]
fn spawn_failure_releases_registration() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wrix"))
        .args(["notify", "run", "--", "/nonexistent/wrix-test-command"])
        .env("XDG_RUNTIME_DIR", root.path())
        .env("XDG_DATA_HOME", root.path())
        .env("HOME", root.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read_dir(sessions(root.path())).unwrap().count(), 0);
}
