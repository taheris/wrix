//! Real packaged daemon, host client and native launch attention acceptance.
#![cfg(all(test, target_os = "linux"))]
use std::{
    env, fs,
    io::Write,
    os::unix::{fs::symlink, net::UnixStream, process::CommandExt},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use wait_timeout::ChildExt;

struct OwnedChild {
    child: Child,
    release: Option<PathBuf>,
    daemon: bool,
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(release) = &self.release {
            fs::write(release, []).unwrap();
        }
        if self.daemon && self.child.try_wait().unwrap().is_none() {
            let pid = rustix::process::Pid::from_raw(self.child.id().try_into().unwrap()).unwrap();
            match rustix::process::kill_process_group(pid, rustix::process::Signal::Term) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => panic!("owned daemon termination: {error}"),
            }
        }
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
                "owned notification child exceeded original cleanup bound"
            );
        }
    }
}

struct Fixture {
    root: tempfile::TempDir,
    sessions: PathBuf,
    runtime: PathBuf,
    log: PathBuf,
    desktop: PathBuf,
    capture: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("runtime");
        let sessions = runtime.join("wrix/sessions");
        let bin = root.path().join("bin");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir(&bin).unwrap();
        symlink(
            env::var_os("WRIX_TEST_NIRI_FIXTURE").unwrap(),
            bin.join("niri"),
        )
        .unwrap();
        Self {
            sessions,
            runtime,
            log: root.path().join("log"),
            desktop: root.path().join("windows"),
            capture: root.path().join("dispatch"),
            root,
            bin,
        }
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.env_clear();
        for name in ["TMPDIR", "LANG", "LC_ALL", "LC_CTYPE"] {
            if let Some(value) = env::var_os(name) {
                command.env(name, value);
            }
        }
        let path = env::join_paths(
            std::iter::once(self.bin.clone())
                .chain(env::split_paths(&env::var_os("PATH").unwrap())),
        )
        .unwrap();
        command
            .env("PATH", path)
            .env("HOME", self.root.path())
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("WINDOWS", &self.desktop)
            .env("LOG", &self.log)
            .env("WRIX_NOTIFY_TEST_DISPATCH_CAPTURE", &self.capture);
        command
    }

    fn desktop(&self, windows: Value) {
        fs::write(&self.desktop, serde_json::to_vec(&windows).unwrap()).unwrap();
    }

    fn register(&self, target: &str, window: u32, pid: u32) {
        fs::write(
            self.sessions.join(format!("{target}.json")),
            json!({
                "focus_target":target, "window_id":window.to_string(),
                "linux_route":{"terminal_pid":pid,"client":null}
            })
            .to_string(),
        )
        .unwrap();
    }

    fn pending(&self) -> Vec<String> {
        let file = self.sessions.join("attention.sqlite");
        if !file.exists() {
            return Vec::new();
        }
        Connection::open(file)
            .unwrap()
            .prepare("SELECT target FROM pending ORDER BY sequence DESC")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn saved_origin(&self) -> Option<Value> {
        Connection::open(self.sessions.join("attention.sqlite"))
            .unwrap()
            .query_row("SELECT route FROM return_position", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .unwrap()
            .map(|value| serde_json::from_str(&value).unwrap())
    }

    fn focused(&self) -> u64 {
        let windows: Value = serde_json::from_slice(&fs::read(&self.desktop).unwrap()).unwrap();
        windows
            .as_array()
            .unwrap()
            .iter()
            .find(|window| window["is_focused"] == true)
            .unwrap()["id"]
            .as_u64()
            .unwrap()
    }

    fn dispatches(&self) -> Vec<Value> {
        if !self.capture.exists() {
            return Vec::new();
        }
        fs::read_to_string(&self.capture)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn daemon(&self, failed_title: Option<&str>) -> OwnedChild {
        let log = fs::File::create(self.root.path().join("daemon.log")).unwrap();
        let mut command = self.command("wrix-notifyd");
        if let Some(title) = failed_title {
            command.env("WRIX_NOTIFY_TEST_FAIL_TITLE", title);
        }
        OwnedChild {
            child: command
                .process_group(0)
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
            release: None,
            daemon: true,
        }
    }

    fn invoke(&self, command: &mut Command) -> Output {
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "notification command in {}: {result:?}",
            self.root.path().display()
        );
        result
    }
}

fn wait_for(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "original notification readiness bound exceeded"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires real Linux packaged notification daemon and host tools"]
fn dispatched_notifications_queue_without_focus_and_unrouted_delivery_unchanged() {
    let fixture = Fixture::new();
    let pid = std::process::id();
    fixture.desktop(json!([
        {"id":1,"pid":pid,"is_focused":false},
        {"id":99,"pid":pid+1,"is_focused":true},
        {"id":2,"pid":pid,"is_focused":false}
    ]));
    fixture.register("agent", 1, pid);
    fixture.register("focused", 99, pid + 1);
    fixture.register("failed-agent", 2, pid);
    let mut daemon = fixture.daemon(Some("failed"));
    let endpoint = fixture.runtime.join("wrix/notify.sock");
    wait_for(Duration::from_secs(5), || {
        assert!(daemon.child.try_wait().unwrap().is_none());
        endpoint.exists()
    });
    let mut stream = UnixStream::connect(endpoint).unwrap();
    for payload in [
        json!({"title":"queued","message":"input","focus_target":"agent","command":"never execute"}),
        json!({"title":"failed","message":"input","focus_target":"failed-agent"}),
        json!({"title":"unrouted","message":"input"}),
        json!({"title":"missing","message":"input","focus_target":"missing"}),
        json!({"title":"suppressed","message":"input","focus_target":"focused"}),
    ] {
        writeln!(stream, "{payload}").unwrap();
    }
    wait_for(Duration::from_secs(15), || fixture.dispatches().len() >= 3);
    drop(stream);
    assert_eq!(
        fixture
            .dispatches()
            .iter()
            .map(|line| line[0].clone())
            .collect::<Vec<_>>(),
        vec![json!("queued"), json!("unrouted"), json!("missing")]
    );
    assert_eq!(fixture.pending(), ["agent"]);
    assert!(!fixture.log.exists(), "notification dispatch stole focus");
    fixture.invoke(&mut fixture.command("wrix-focus-notification"));
    assert_eq!(fixture.pending(), Vec::<String>::new());
    assert_eq!(fs::read_to_string(&fixture.log).unwrap(), "1\n");
    fixture.invoke(fixture.command("wrix-focus-notification").arg("--return"));
    assert_eq!(fixture.focused(), 99);
    assert!(fixture.saved_origin().is_none());
    assert_eq!(fs::read_to_string(&fixture.log).unwrap(), "1\n99\n");
}

#[test]
#[ignore = "requires real Linux packaged native Wrix and notification host tools"]
fn native_launch_host_delivery_and_navigation() {
    let fixture = Fixture::new();
    let pid = std::process::id();
    fixture.desktop(json!([
        {"id":1,"pid":pid,"is_focused":true},
        {"id":99,"pid":pid+1,"is_focused":false}
    ]));
    let output = fixture.root.path().join("target");
    let release = fixture.root.path().join("release");
    let mut missing = fixture.command("wrix-notify-host");
    let mut child = missing
        .process_group(0)
        .args(["missing daemon", "complete"])
        .env("WRIX_NOTIFY_TCP", "invalid:1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if child
        .wait_timeout(Duration::from_secs(5))
        .unwrap()
        .is_none()
    {
        let pid = rustix::process::Pid::from_raw(child.id().try_into().unwrap()).unwrap();
        rustix::process::kill_process_group(pid, rustix::process::Signal::Kill).unwrap();
        child.wait().unwrap();
        panic!("host client exceeded original five-second bound");
    }
    let missing = child.wait_with_output().unwrap();
    assert!(missing.status.success());
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("socket not found")
    );
    let _daemon = fixture.daemon(None);
    let mut command = fixture.command(&env::var("WRIX_NATIVE_TEST_BIN").unwrap());
    let mut agent = OwnedChild {
        child: command.process_group(0).args(["notify","run","--","sh","-c",
            "printf '%s' \"$WRIX_FOCUS_TARGET\" > \"$OUTPUT\"; while [ ! -f \"$RELEASE\" ]; do sleep 0.02; done"])
            .env("WRIX_NOTIFY_TCP", "invalid:1").env("OUTPUT", &output).env("RELEASE", &release)
            .spawn().unwrap(),
        release: Some(release.clone()),
        daemon: false,
    };
    wait_for(Duration::from_secs(10), || {
        output.exists()
            && !fs::read(&output).unwrap().is_empty()
            && fixture.runtime.join("wrix/notify.sock").exists()
    });
    let target = fs::read_to_string(&output).unwrap();
    let name: String = target
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
                char::from(byte)
            } else {
                '-'
            }
        })
        .collect();
    let record: Value =
        serde_json::from_slice(&fs::read(fixture.sessions.join(format!("{name}.json"))).unwrap())
            .unwrap();
    assert_eq!(record["window_id"], "1");
    assert_eq!(record["leased"], true);
    fixture.invoke(
        fixture
            .command("wrix-notify-host")
            .args(["suppressed", "complete"])
            .env("WRIX_FOCUS_TARGET", &target)
            .env("WRIX_NOTIFY_TCP", "invalid:1"),
    );
    thread::sleep(Duration::from_millis(500));
    assert!(!fixture.capture.exists());
    assert_eq!(fixture.pending(), Vec::<String>::new());
    fixture.desktop(json!([
        {"id":1,"pid":pid,"is_focused":false},
        {"id":99,"pid":pid+1,"is_focused":true}
    ]));
    fixture.invoke(
        fixture
            .command("wrix-notify-host")
            .args(["Codex", "complete"])
            .env("WRIX_FOCUS_TARGET", &target)
            .env("WRIX_NOTIFY_TCP", "invalid:1"),
    );
    wait_for(Duration::from_secs(10), || {
        fixture.pending() == [target.as_str()]
    });
    assert_eq!(fixture.dispatches(), [json!(["Codex", "complete"])]);
    fixture.invoke(&mut fixture.command("wrix-focus-notification"));
    assert_eq!(fixture.focused(), 1);
    fixture.invoke(fixture.command("wrix-focus-notification").arg("--return"));
    assert_eq!(fixture.focused(), 99);
    fs::write(&release, []).unwrap();
    let status = agent
        .child
        .wait_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert!(status.success());
    fixture.invoke(
        fixture
            .command("wrix-focus-notification")
            .args(["--enqueue", &target]),
    );
    assert_eq!(fixture.pending(), Vec::<String>::new());
    fixture.invoke(
        fixture
            .command("wrix-notify-host")
            .args(["exited", "complete"])
            .env("WRIX_FOCUS_TARGET", &target)
            .env("WRIX_NOTIFY_TCP", "invalid:1"),
    );
    wait_for(Duration::from_secs(10), || fixture.dispatches().len() == 2);
    assert_eq!(fixture.pending(), Vec::<String>::new());
}
