//! Conformance of the external desktop command fixture.
#![cfg(test)]
use serde_json::{Value, json};
use std::{
    env, fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use wait_timeout::ChildExt;

struct HeldChild {
    child: Child,
    release: PathBuf,
}

impl Drop for HeldChild {
    fn drop(&mut self) {
        fs::write(&self.release, []).unwrap();
        if self
            .child
            .wait_timeout(Duration::from_secs(5))
            .unwrap()
            .is_none()
        {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
            assert!(
                thread::panicking(),
                "held external fixture failed to stop after release"
            );
        }
    }
}

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("windows"),
            json!([
                {"id":1,"pid":10,"is_focused":false},
                {"id":99,"pid":20,"is_focused":true}
            ])
            .to_string(),
        )
        .unwrap();
        Self(root)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env::var_os("WRIX_TEST_NIRI_FIXTURE").unwrap());
        command
            .env_clear()
            .args(args)
            .env("WINDOWS", self.0.path().join("windows"))
            .env("LOG", self.0.path().join("log"))
            .env("FOCUS_READY", self.0.path().join("ready"))
            .env("FOCUS_RELEASE", self.0.path().join("release"));
        command
    }

    fn windows(&self) -> Value {
        serde_json::from_slice(&fs::read(self.0.path().join("windows")).unwrap()).unwrap()
    }
}

#[test]
#[ignore = "requires immutable packaged external niri fixture"]
fn desktop_queries_preserve_exact_external_json_contract() {
    let fixture = Fixture::new();
    let result = fixture.command(&["msg", "-j", "windows"]).output().unwrap();
    assert!(result.status.success());
    assert_eq!(result.stderr, b"");
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap(),
        fixture.windows()
    );
    let result = fixture
        .command(&["msg", "-j", "focused-window"])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap(),
        json!({"id":99})
    );
}

#[test]
#[ignore = "requires immutable packaged external niri fixture"]
fn focus_changes_only_selected_window_and_appends_target_log() {
    let fixture = Fixture::new();
    let result = fixture
        .command(&["msg", "action", "focus-window", "--id", "1"])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"");
    assert_eq!(result.stderr, b"");
    assert_eq!(
        fixture.windows(),
        json!([
            {"id":1,"pid":10,"is_focused":true},
            {"id":99,"pid":20,"is_focused":false}
        ])
    );
    assert_eq!(
        fs::read_to_string(fixture.0.path().join("log")).unwrap(),
        "1\n"
    );
}

#[test]
#[ignore = "requires immutable packaged external niri fixture"]
fn rejected_focus_and_unknown_arguments_do_not_change_desktop() {
    let fixture = Fixture::new();
    let before = fixture.windows();
    let result = fixture
        .command(&["msg", "action", "focus-window", "--id", "1"])
        .env("FAIL_FOCUS_WINDOW", "1")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(72));
    let result = fixture.command(&["unexpected"]).output().unwrap();
    assert_eq!(result.status.code(), Some(64));
    assert_eq!(fixture.windows(), before);
    assert!(!fixture.0.path().join("log").exists());
}

#[test]
#[ignore = "requires immutable packaged external niri fixture"]
fn held_focus_reports_ready_and_changes_desktop_only_after_release() {
    let fixture = Fixture::new();
    let before = fixture.windows();
    let mut held = HeldChild {
        child: fixture
            .command(&["msg", "action", "focus-window", "--id", "1"])
            .env("HOLD_FOCUS_WINDOW", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
        release: fixture.0.path().join("release"),
    };
    let child = &mut held.child;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fixture.0.path().join("ready").exists() {
        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            child.kill().unwrap();
            let status = child.wait().unwrap();
            panic!("external fixture did not become ready: {status:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(child.try_wait().unwrap().is_none());
    assert_eq!(fixture.windows(), before);
    assert!(!fixture.0.path().join("log").exists());
    fs::write(fixture.0.path().join("release"), []).unwrap();
    if child
        .wait_timeout(Duration::from_secs(5))
        .unwrap()
        .is_none()
    {
        child.kill().unwrap();
        child.wait().unwrap();
        panic!("external fixture ignored release");
    }
    assert!(child.wait().unwrap().success());
    assert_eq!(fixture.windows()[0]["is_focused"], true);
    assert_eq!(
        fs::read_to_string(fixture.0.path().join("log")).unwrap(),
        "1\n"
    );
}
