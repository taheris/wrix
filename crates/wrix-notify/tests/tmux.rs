#![cfg(test)]
#![cfg(target_os = "linux")]

use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader},
    num::NonZeroU32,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use wrix_notify::{
    attention,
    navigation::{Attached, Navigator, Outcome},
    process::{Error, Host, Runner},
    route::{Platform, Route, Socket, Target},
};

const TERMINAL: &str = r#"
import os, pty, select, subprocess, sys
master, slave = pty.openpty()
env = dict(os.environ, TERM="xterm")
env.pop("TMUX", None)
env.pop("TMUX_PANE", None)
client = subprocess.Popen(["tmux", "-S", sys.argv[1], "attach-session", "-t", sys.argv[2]],
                          stdin=slave, stdout=slave, stderr=slave, env=env)
print(os.getpid(), flush=True)
while client.poll() is None:
    if select.select([master], [], [], 0.1)[0]:
        os.read(master, 4096)
client.wait(timeout=5)
os.close(master)
os.close(slave)
"#;

struct Terminal {
    socket: Socket,
    _cleanup: Cleanup,
    pid: NonZeroU32,
    client: Attached,
}

impl Terminal {
    fn new(root: &Path, name: &str) -> Self {
        let socket: Socket = root
            .join(format!("{name} socket.sock"))
            .to_str()
            .unwrap()
            .to_owned()
            .try_into()
            .unwrap();
        let output = Command::new("tmux")
            .args([
                "-S",
                socket.as_str(),
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                name,
                "sleep 300",
            ])
            .env_remove("TMUX")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut proxy = Command::new("python3")
            .args(["-c", TERMINAL, socket.as_str(), name])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(proxy.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let pid: NonZeroU32 = line.trim().parse().unwrap();
        assert_eq!(pid.get(), proxy.id());
        let cleanup = Cleanup {
            socket: socket.clone(),
            proxy,
        };
        let navigator = Navigator::new(Host, PathBuf::from("/proc"));
        let deadline = Instant::now() + Duration::from_secs(5);
        let client = loop {
            if let Some(clients) = navigator.clients(&socket)
                && let Some(client) = clients.first()
            {
                break client.clone();
            }
            assert!(
                Instant::now() < deadline,
                "terminal client did not attach: {:?}",
                Host.run(
                    "tmux",
                    &[
                        "-S",
                        socket.as_str(),
                        "list-clients",
                        "-F",
                        wrix_notify::navigation::CLIENT_FORMAT
                    ]
                )
            );
            thread::sleep(Duration::from_millis(10));
        };
        Self {
            socket,
            _cleanup: cleanup,
            pid,
            client,
        }
    }

    fn call(&self, args: &[&str]) -> String {
        let mut arguments = vec!["-S", self.socket.as_str()];
        arguments.extend_from_slice(args);
        Host.run("tmux", &arguments).unwrap()
    }

    fn route(&self, window: u32, pane: &str) -> Route {
        Route::parse(
            &json!({"window_id":window.to_string(),"linux_route":{
            "terminal_pid":self.pid,"client":self.client.identity},
            "tmux_socket":self.socket,"tmux_target":pane,"tmux_window":self.client.window}),
            Platform::Linux,
        )
        .unwrap()
    }
}

struct Cleanup {
    socket: Socket,
    proxy: Child,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        match Command::new("tmux")
            .args(["-S", self.socket.as_str(), "kill-server"])
            .output()
        {
            Ok(_) => {}
            Err(error) => tracing::warn!(%error,"test tmux cleanup failed"),
        }
        if let Err(error) = self.proxy.wait() {
            tracing::warn!(%error,"test terminal cleanup failed");
        }
    }
}

#[derive(Clone)]
struct Desktop(Arc<Mutex<Value>>);
impl Runner for Desktop {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, Error> {
        if program != "niri" {
            return Host.run(program, args);
        }
        let mut windows = self.0.lock().unwrap();
        match args {
            ["msg", "-j", "windows"] => Ok(windows.to_string()),
            ["msg", "action", "focus-window", "--id", id] => {
                for window in windows.as_array_mut().unwrap() {
                    window["is_focused"] =
                        json!(window["id"].as_u64() == Some(id.parse().unwrap()));
                }
                Ok(String::new())
            }
            _ => Err(Error::Io(std::io::Error::other(
                "unexpected desktop command",
            ))),
        }
    }
}

struct Fixture {
    _lock: fs::File,
    root: TempDir,
    directory: PathBuf,
    desktop: Desktop,
    navigator: Navigator<Desktop>,
}
impl Fixture {
    fn new() -> Self {
        static LOG: std::sync::Once = std::sync::Once::new();
        LOG.call_once(|| {
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::DEBUG)
                .with_test_writer()
                .init();
        });
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(std::env::temp_dir().join("wrix-notify-tmux-tests.lock"))
            .unwrap();
        lock.lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("sessions");
        fs::create_dir(&directory).unwrap();
        let desktop = Desktop(Arc::new(Mutex::new(json!([]))));
        let navigator = Navigator::new(desktop.clone(), PathBuf::from("/proc"));
        Self {
            _lock: lock,
            root,
            directory,
            desktop,
            navigator,
        }
    }
    fn windows(&self, value: Value) {
        *self.desktop.0.lock().unwrap() = value;
    }
    fn register(&self, target: &str, route: &Route) {
        let mut value = route.saved();
        value["focus_target"] = json!(target);
        let target: Target = target.to_owned().try_into().unwrap();
        fs::write(self.directory.join(target.filename()), value.to_string()).unwrap();
    }
    fn enqueue(&self, target: &str) {
        attention::enqueue(
            &self.directory,
            &target.to_owned().try_into().unwrap(),
            Platform::Linux,
        )
        .unwrap();
    }
    fn focus(&self) {
        attention::focus(&self.directory, Platform::Linux, &self.navigator).unwrap();
    }
    fn returning(&self) {
        attention::return_to_origin(&self.directory, Platform::Linux, &self.navigator).unwrap();
    }
    fn saved(&self) -> Option<Value> {
        use rusqlite::OptionalExtension;
        attention::open(&self.directory)
            .unwrap()
            .query_row("SELECT route FROM return_position", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .unwrap()
            .map(|text| serde_json::from_str(&text).unwrap())
    }
}

#[test]
fn terminal_fixture_matches_real_client_ancestry_active_position_and_socket_discovery() {
    let f = Fixture::new();
    let t = Terminal::new(f.root.path(), "fixture");
    assert_eq!(
        t.client.pane.as_str(),
        t.call(&["display-message", "-p", "-t", "fixture", "#{pane_id}"])
    );
    assert_eq!(
        t.client.window.as_str(),
        t.call(&["display-message", "-p", "-t", "fixture", "#{window_id}"])
    );
    assert!(f.navigator.client_origin_matches(&t.client.identity, t.pid));
    assert!(
        f.navigator
            .sockets(&f.directory)
            .unwrap()
            .contains(&t.socket)
    );
}

#[test]
fn cross_workspace_then_same_terminal_pane_jump_returns_to_latest_origin_pane() {
    let f = Fixture::new();
    let a = Terminal::new(f.root.path(), "workspace-a");
    let b = Terminal::new(f.root.path(), "workspace-b");
    let pane = b.call(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        b.client.pane.as_str(),
        "sleep 300",
    ]);
    f.windows(
        json!([{"id":1,"pid":a.pid,"is_focused":true},{"id":2,"pid":b.pid,"is_focused":false}]),
    );
    f.register("first", &b.route(2, b.client.pane.as_str()));
    f.register("second", &b.route(2, &pane));
    f.enqueue("first");
    f.focus();
    let saved = f.saved().unwrap();
    assert_eq!(saved["window_id"], "1");
    assert_eq!(saved["tmux_target"], a.client.pane.as_str());
    f.enqueue("second");
    f.focus();
    let saved = f.saved().unwrap();
    assert_eq!(saved["window_id"], "2");
    assert_eq!(saved["tmux_target"], b.client.pane.as_str());
    assert_eq!(saved["tmux_window"], b.client.window.as_str());
    assert_eq!(
        b.call(&[
            "display-message",
            "-p",
            "-t",
            b.client.identity.session.as_str(),
            "#{pane_id}"
        ]),
        pane
    );
    f.returning();
    assert!(f.saved().is_none());
    assert_eq!(
        b.call(&[
            "display-message",
            "-p",
            "-t",
            b.client.identity.session.as_str(),
            "#{pane_id}"
        ]),
        b.client.pane.as_str()
    );
    assert_eq!(
        a.call(&[
            "display-message",
            "-p",
            "-t",
            a.client.identity.session.as_str(),
            "#{pane_id}"
        ]),
        a.client.pane.as_str()
    );
}

#[test]
fn renamed_session_and_reindexed_window_keep_stable_pane_routing() {
    let f = Fixture::new();
    let t = Terminal::new(f.root.path(), "host");
    let pane = t.call(&[
        "new-window",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "host:1",
        "sleep 300",
    ]);
    t.call(&["select-window", "-t", "host:0"]);
    f.windows(json!([{"id":1,"pid":t.pid,"is_focused":true}]));
    let route = t.route(1, &pane);
    t.call(&["rename-session", "-t", "host", "renamed"]);
    t.call(&["move-window", "-s", "renamed:1", "-t", "renamed:5"]);
    assert_eq!(f.navigator.navigate(&route, false), Outcome::Success);
    assert_eq!(
        t.call(&["display-message", "-p", "-t", "renamed", "#{window_index}"]),
        "5"
    );
    t.call(&["set-option", "-w", "-t", &pane, "remain-on-exit", "on"]);
    t.call(&["respawn-pane", "-k", "-t", &pane, "exit 0"]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while t.call(&["display-message", "-p", "-t", &pane, "#{pane_dead}"]) != "1" {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(f.navigator.navigate(&route, false), Outcome::Stale);
}

#[test]
fn return_restores_only_the_explicit_client_to_its_original_session() {
    let f = Fixture::new();
    let t = Terminal::new(f.root.path(), "origin");
    t.call(&["new-session", "-d", "-s", "destination", "sleep 300"]);
    f.windows(json!([{"id":1,"pid":t.pid,"is_focused":true}]));
    let route = t.route(1, t.client.pane.as_str());
    t.call(&[
        "switch-client",
        "-c",
        t.client.identity.name.as_str(),
        "-t",
        "destination",
    ]);
    assert_eq!(f.navigator.navigate(&route, false), Outcome::Stale);
    assert_eq!(f.navigator.navigate(&route, true), Outcome::Success);
    assert_eq!(
        t.call(&["list-clients", "-F", "#{session_id}"]),
        t.client.identity.session.as_str()
    );
}

#[test]
fn shared_pid_tmux_origins_are_not_saved_as_arbitrary_windows() {
    let f = Fixture::new();
    let t = Terminal::new(f.root.path(), "origin");
    f.windows(
        json!([{"id":1,"pid":t.pid,"is_focused":true},{"id":2,"pid":t.pid,"is_focused":false}]),
    );
    assert!(f.navigator.capture_origin(&f.directory).is_none());
}
