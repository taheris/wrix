#![cfg(test)]
#![cfg(any(target_os = "linux", target_os = "macos"))]

use serde_json::{Value, json};
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};
use wait_timeout::ChildExt;

#[derive(Clone, Copy)]
enum Server {
    Default,
    Named,
}

struct Client {
    child: Child,
    _master: fs::File,
    _slave: fs::File,
}
impl Client {
    fn terminate(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let pid =
                rustix::process::Pid::from_raw(i32::try_from(self.child.id()).unwrap()).unwrap();
            match rustix::process::kill_process(pid, rustix::process::Signal::TERM) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => panic!("cannot stop owned tmux client: {error}"),
            }
        }
        assert!(
            self.child
                .wait_timeout(Duration::from_secs(5))
                .unwrap()
                .is_some()
        );
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        if self
            .child
            .wait_timeout(Duration::from_secs(5))
            .unwrap()
            .is_none()
        {
            self.child.kill().unwrap();
            assert!(
                self.child
                    .wait_timeout(Duration::from_secs(5))
                    .unwrap()
                    .is_some()
            );
        }
    }
}

struct Servers {
    root: PathBuf,
    named: PathBuf,
    clients: Vec<Client>,
}
impl Servers {
    fn new(root: PathBuf) -> Self {
        fs::create_dir_all(root.join("default-server")).unwrap();
        Self {
            named: root.join("named socket,focus"),
            root,
            clients: Vec::new(),
        }
    }
    fn command(&self, server: Server) -> Command {
        let mut command = Command::new("tmux");
        command
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("TMUX_TMPDIR", self.root.join("default-server"));
        if matches!(server, Server::Named) {
            command.arg("-S").arg(&self.named);
        }
        command
    }
    fn tmux(&self, server: Server, args: &[&str]) -> String {
        let output = self.command(server).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
    fn attach(&mut self, server: Server, session: &str, count: usize) -> usize {
        let pty = nix::pty::openpty(None, None).unwrap();
        rustix::io::fcntl_setfd(&pty.master, rustix::io::FdFlags::CLOEXEC).unwrap();
        rustix::io::fcntl_setfd(&pty.slave, rustix::io::FdFlags::CLOEXEC).unwrap();
        let master = fs::File::from(pty.master);
        let slave = fs::File::from(pty.slave);
        let child = self
            .command(server)
            .args(["attach-session", "-t", session])
            .env("TERM", "xterm")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .unwrap();
        self.clients.push(Client {
            child,
            _master: master,
            _slave: slave,
        });
        wait_for(|| {
            self.tmux(server, &["list-clients", "-F", "#{client_session}"])
                .lines()
                .count()
                == count
        });
        self.clients.len() - 1
    }
}
impl Drop for Servers {
    fn drop(&mut self) {
        for server in [Server::Named, Server::Default] {
            let _ = self.command(server).arg("kill-server").output();
        }
    }
}

fn wait_for(mut predicate: impl FnMut() -> bool) {
    for _ in 0..100 {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for tmux or notification daemon");
}

fn identity(rows: &str, session: &str) -> Value {
    let fields = rows
        .lines()
        .map(|row| row.split('|').collect::<Vec<_>>())
        .find(|fields| fields.last().copied() == Some(session))
        .unwrap_or_else(|| panic!("missing client for session {session:?} in rows {rows:?}"));
    assert_eq!(fields.len(), 4, "malformed tmux client identity");
    json!({"name": fields[0], "pid": fields[1].parse::<u32>().unwrap(), "tty": fields[2], "session": fields[3]})
}

#[cfg(target_os = "linux")]
fn send(runtime: &Path, payload: &[u8]) {
    use rustix::net::{
        AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with,
    };
    let fd = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    let address = SocketAddrUnix::new(runtime.join("wrix/notify.sock")).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match connect(&fd, &address) {
            Ok(()) | Err(rustix::io::Errno::ISCONN) => break,
            Err(
                rustix::io::Errno::AGAIN
                | rustix::io::Errno::INPROGRESS
                | rustix::io::Errno::ALREADY,
            ) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "notification connect exceeded three seconds"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("notification connection failed: {error}"),
        }
    }
    rustix::fs::fcntl_setfl(&fd, rustix::fs::OFlags::empty()).unwrap();
    let mut stream = std::os::unix::net::UnixStream::from(fd);
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream.write_all(payload).unwrap();
}

#[cfg(target_os = "macos")]
fn send(_runtime: &Path, payload: &[u8]) {
    let mut stream = std::net::TcpStream::connect_timeout(
        &"192.168.64.1:5959".parse().unwrap(),
        Duration::from_secs(3),
    )
    .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream.write_all(payload).unwrap();
}

struct Workflow {
    runtime: PathBuf,
    capture: PathBuf,
    log: PathBuf,
    record_path: PathBuf,
    record: Value,
    suppressions: usize,
}
impl Workflow {
    fn check(&mut self, title: &str, suppressed: bool) {
        fs::write(&self.record_path, serde_json::to_vec(&self.record).unwrap()).unwrap();
        fs::write(&self.capture, "").unwrap();
        let payload = format!(
            "{}\n",
            json!({"title":title,"message":"focus check","focus_target":"visible:0.0"})
        );
        send(&self.runtime, payload.as_bytes());
        if suppressed {
            self.suppressions += 1;
            wait_for(|| {
                fs::read_to_string(&self.log)
                    .unwrap()
                    .matches("suppressed (terminal focused)")
                    .count()
                    == self.suppressions
            });
            assert!(
                fs::read_to_string(&self.capture).unwrap().is_empty(),
                "{title}: {}",
                fs::read_to_string(&self.log).unwrap()
            );
        } else {
            wait_for(|| !fs::read_to_string(&self.capture).unwrap().is_empty());
            let dispatches: Vec<Value> = fs::read_to_string(&self.capture)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let expected = if cfg!(target_os = "macos") {
                json!(["-title", title, "-message", "focus check"])
            } else {
                json!([title, "focus check"])
            };
            assert_eq!(
                dispatches,
                vec![expected],
                "{title}: {}",
                fs::read_to_string(&self.log).unwrap()
            );
        }
        println!("PASS: {title}");
    }
}

#[test]
fn pty_fixture_attaches_and_reaps_its_actual_tmux_client() {
    let root = tempfile::tempdir().unwrap();
    let mut servers = Servers::new(root.path().to_owned());
    servers.tmux(
        Server::Named,
        &[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "visible",
            "sleep 120",
        ],
    );
    let client = servers.attach(Server::Named, "visible", 1);
    assert_eq!(
        servers.tmux(Server::Named, &["list-clients", "-F", "#{client_session}"]),
        "visible"
    );
    servers.clients[client].terminate();
    wait_for(|| {
        servers.tmux(
            Server::Named,
            &[
                "display-message",
                "-p",
                "-t",
                "visible:0.0",
                "#{session_attached}",
            ],
        ) == "0"
    });
}

#[test]
fn client_identity_preserves_printable_framing_and_session_selection() {
    assert_eq!(
        identity("other|21|/dev/pts/2|$1\nvisible|20|/dev/pts/1|$0", "$0"),
        json!({"name":"visible","pid":20,"tty":"/dev/pts/1","session":"$0"})
    );
}

#[test]
#[should_panic(expected = "malformed tmux client identity")]
fn malformed_client_identity_cannot_be_used_as_fixture_evidence() {
    identity("missing|columns", "columns");
}

#[test]
#[ignore = "requires packaged daemon, native dispatch fixture and isolated host transport"]
fn packaged_daemon_uses_registered_tmux_target_without_cross_client_fallback() {
    let required = |key| PathBuf::from(env::var_os(key).unwrap());
    let root = required("WRIX_TEST_TMUX_ROOT");
    let runtime = required("WRIX_TEST_TMUX_RUNTIME");
    let capture = required("WRIX_TEST_TMUX_CAPTURE");
    let log = required("WRIX_TEST_TMUX_LOG");
    let record_path = runtime.join(if cfg!(target_os = "macos") {
        "home/.local/share/wrix/sessions/visible-0-0.json"
    } else {
        "wrix/sessions/visible-0-0.json"
    });
    let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    let mut servers = Servers::new(root);
    record["tmux_target"] = json!("visible:0.0");
    record["tmux_socket"] = json!(servers.named);
    let mut workflow = Workflow {
        runtime,
        capture,
        log,
        record_path,
        record,
        suppressions: 0,
    };
    for server in [Server::Default, Server::Named] {
        servers.tmux(
            server,
            &[
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "visible",
                "sleep 120",
            ],
        );
    }
    servers.attach(Server::Default, "visible", 1);
    thread::sleep(Duration::from_millis(1100));
    servers.tmux(
        Server::Default,
        &["new-session", "-d", "-s", "other", "sleep 120"],
    );
    servers.attach(Server::Default, "other", 2);
    assert_eq!(
        servers.tmux(
            Server::Default,
            &["display-message", "-p", "#{session_name}"]
        ),
        "other"
    );
    servers.tmux(
        Server::Named,
        &["new-session", "-d", "-s", "other", "sleep 120"],
    );
    let visible_client = servers.attach(Server::Named, "visible", 1);
    thread::sleep(Duration::from_millis(1100));
    servers.attach(Server::Named, "other", 2);
    assert_eq!(
        servers.tmux(Server::Named, &["display-message", "-p", "#{session_name}"]),
        "other"
    );
    workflow.record["tmux_target"] = json!(servers.tmux(
        Server::Named,
        &["display-message", "-p", "-t", "visible:0.0", "#{pane_id}"]
    ));
    let rows = servers.tmux(
        Server::Named,
        &[
            "list-clients",
            "-F",
            "#{client_name}|#{client_pid}|#{client_tty}|#{session_id}",
        ],
    );
    let session = servers.tmux(
        Server::Named,
        &[
            "display-message",
            "-p",
            "-t",
            "visible:0.0",
            "#{session_id}",
        ],
    );
    workflow.record[if cfg!(target_os = "macos") {
        "omniwm_route"
    } else {
        "linux_route"
    }]["client"] = identity(&rows, &session);
    workflow.check(
        "focused target ignores another client and the default server",
        true,
    );
    let window_focus = required("WRIX_NOTIFY_TEST_WINDOW_FOCUS");
    fs::write(&window_focus, "false\n").unwrap();
    workflow.check(
        "active origin pane in a background terminal window notifies",
        false,
    );
    fs::remove_file(&window_focus).unwrap();
    workflow.check("unavailable terminal focus information notifies", false);
    fs::write(&window_focus, "true\n").unwrap();
    workflow
        .record
        .as_object_mut()
        .unwrap()
        .remove("tmux_socket");
    workflow.check(
        "socketless pane registration leaves delivery unsuppressed",
        false,
    );
    workflow.record["tmux_socket"] = json!(servers.named);
    servers.tmux(
        Server::Named,
        &["split-window", "-t", "visible:0.0", "sleep 120"],
    );
    workflow.check("inactive target pane notifies", false);
    servers.tmux(Server::Named, &["select-pane", "-t", "visible:0.0"]);
    workflow.check("reselected target pane suppresses", true);
    servers.tmux(
        Server::Named,
        &["new-window", "-t", "visible:", "sleep 120"],
    );
    workflow.check(
        "inactive target window notifies even with an active pane",
        false,
    );
    servers.tmux(Server::Named, &["select-window", "-t", "visible:0"]);
    workflow.check("reselected target window suppresses", true);
    if cfg!(target_os = "linux") {
        let ambiguous_client = servers.attach(Server::Named, "visible", 3);
        workflow.check(
            "multiple clients in target session leave delivery unsuppressed",
            false,
        );
        servers.clients[ambiguous_client].terminate();
        wait_for(|| {
            servers
                .tmux(Server::Named, &["list-clients", "-F", "#{client_session}"])
                .lines()
                .count()
                == 2
        });
        workflow.check("unique originating client restores focus suppression", true);
    }
    servers.clients[visible_client].terminate();
    wait_for(|| {
        servers.tmux(
            Server::Named,
            &[
                "display-message",
                "-p",
                "-t",
                "visible:0.0",
                "#{session_attached}",
            ],
        ) == "0"
    });
    workflow.check("detached target session notifies", false);
    servers.tmux(Server::Named, &["kill-session", "-t", "visible"]);
    workflow.check(
        "missing target notifies without using another client",
        false,
    );
    workflow.record["tmux_socket"] = json!(servers.root.join("missing.socket"));
    workflow.check(
        "unavailable registered socket notifies without default-server fallback",
        false,
    );
}
