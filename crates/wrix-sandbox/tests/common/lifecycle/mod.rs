use std::{
    fs, io,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::json;
use wrix_sandbox::image::Runtime;

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const HOLD_INTERVAL: Duration = Duration::from_millis(200);
const IMAGE: &str = "localhost/wrix-test:latest";

pub struct Fixture {
    root: tempfile::TempDir,
    pub profile: PathBuf,
    pub spawn: PathBuf,
    listener: TcpListener,
}

impl Fixture {
    pub fn new() -> TestResult<Self> {
        let root = tempfile::Builder::new()
            .prefix("spawn-lifecycle")
            .tempdir()?;
        let profile = root.path().join("profile.json");
        let spawn = root.path().join("spawn.json");
        let workspace = root.path().join("workspace");
        let bin = root.path().join("bin");
        for dir in [&workspace, &bin, &root.path().join("home")] {
            fs::create_dir_all(dir)?;
        }
        for name in ["podman", "container", "route"] {
            let path = bin.join(name);
            fs::write(&path, include_str!("../../fixtures/lifecycle-runtime.sh"))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
        fs::write(root.path().join("deploy-key"), "fixture private key\n")?;
        let source_kind = if cfg!(target_os = "macos") {
            "docker-archive"
        } else {
            "nix-descriptor"
        };
        fs::write(
            &profile,
            serde_json::to_vec(&json!({
                "schema": 1, "system": "test", "profile": {"name": "base"},
                "image": {
                    "ref": IMAGE, "source": "/missing/image-source",
                    "source_kind": source_kind, "digest": digest()
                },
                "agent": {"kind": "direct"},
                "services": {"beads": {"enable": false}, "nix_cache": {"enable": false}}
            }))?,
        )?;
        fs::write(
            &spawn,
            serde_json::to_vec(&json!({
                "workspace": workspace, "env": [], "agent_args": ["literal argument", "--help"],
                "git": {"deploy": false, "sign": false}, "mounts": []
            }))?,
        )?;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            root,
            profile,
            spawn,
            listener,
        })
    }

    pub fn configure(&self, command: &mut Command, exit_code: u8) -> TestResult {
        let path = std::env::join_paths(std::iter::once(self.root.path().join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").ok_or("PATH is missing")?),
        ))?;
        command
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.root.path().join("home"))
            .env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("WRIX_IMAGE_KEEP_FILE", self.root.path().join("mru.json"))
            .env("WRIX_DEPLOY_KEY", self.root.path().join("deploy-key"))
            .env("WRIX_GIT_SIGN", "0")
            .env("GIT_AUTHOR_NAME", "Wrix Test")
            .env("GIT_AUTHOR_EMAIL", "wrix@example.test")
            .env("GIT_COMMITTER_NAME", "Wrix Test")
            .env("GIT_COMMITTER_EMAIL", "wrix@example.test")
            .env("WRIX_TEST_DIGEST", digest())
            .env("WRIX_TEST_ARGV", self.root.path().join("argv"))
            .env("WRIX_TEST_EXIT_CODE", exit_code.to_string())
            .env("WRIX_TEST_PROFILE_CONFIG", &self.profile)
            .env("WRIX_TEST_SPAWN_CONFIG", &self.spawn)
            .env(
                "WRIX_TEST_CONTROL_PORT",
                self.listener.local_addr()?.port().to_string(),
            )
            .stdin(Stdio::null())
            .stdout(fs::File::create(self.root.path().join("stdout"))?)
            .stderr(fs::File::create(self.root.path().join("stderr"))?);
        Ok(())
    }

    pub fn assert_foreground(
        &self,
        mut command: Command,
        stdio: bool,
        exit_code: u8,
    ) -> TestResult {
        self.configure(&mut command, exit_code)?;
        command.env("WRIX_TEST_STDIO", if stdio { "1" } else { "0" });
        let mut child = Process::spawn(&mut command)?;
        let mut control = self.accept(&mut child)?;
        assert_eq!(
            control.receive()?,
            format!("ready {}", runtime_name(native_runtime()))
        );
        let argv = self.argv()?;
        let image = argv
            .iter()
            .position(|arg| arg == IMAGE)
            .ok_or("image argument missing")?;
        let options = &argv[..image];
        assert_eq!(options.first().map(String::as_str), Some("run"));
        assert_eq!(
            options.iter().filter(|arg| arg.as_str() == "-i").count(),
            usize::from(stdio)
        );
        assert_eq!(
            options
                .iter()
                .filter(|arg| arg.as_str() == "WRIX_STDIO=1")
                .count(),
            usize::from(stdio)
        );
        for option in options {
            assert!(
                !(matches!(option.split('=').next(), Some("--tty" | "--detach"))
                    || (option.starts_with('-')
                        && !option.starts_with("--")
                        && option[1..].contains(['t', 'd']))),
                "unexpected TTY/detach option: {option}"
            );
        }
        assert_eq!(&argv[image + 1..], ["literal argument", "--help"]);
        self.assert_held(&mut child, &mut control)?;
        control.send(Request::Release)?;
        assert_eq!(control.receive()?, format!("finished {exit_code}"));
        let status = child.finish()?;
        assert_eq!(
            status.code(),
            Some(i32::from(exit_code)),
            "{}",
            self.diagnostics()?
        );
        Ok(())
    }

    pub fn launched_argv(
        &self,
        mut command: Command,
        environment: &[(&str, &str)],
    ) -> TestResult<Vec<String>> {
        self.configure(&mut command, 0)?;
        command.envs(environment.iter().copied());
        let mut child = Process::spawn(&mut command)?;
        let mut control = self.accept(&mut child)?;
        assert_eq!(
            control.receive()?,
            format!("ready {}", runtime_name(native_runtime()))
        );
        let argv = self.argv()?;
        control.send(Request::Release)?;
        assert_eq!(control.receive()?, "finished 0");
        assert!(child.finish()?.success(), "{}", self.diagnostics()?);
        Ok(argv)
    }

    fn accept(&self, child: &mut Process) -> TestResult<Control> {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => return Control::new(stream),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
            if let Some(status) = child.child.try_wait()? {
                return Err(format!(
                    "launcher exited before runtime readiness: {status}: {}",
                    self.diagnostics()?
                )
                .into());
            }
            if Instant::now() >= deadline {
                return Err(format!("runtime readiness timed out: {}", self.diagnostics()?).into());
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn assert_held(&self, child: &mut Process, control: &mut Control) -> TestResult {
        let deadline = Instant::now() + HOLD_INTERVAL;
        loop {
            control.send(Request::Probe)?;
            assert_eq!(control.receive()?, "held");
            assert!(
                child.child.try_wait()?.is_none(),
                "launcher exited while runtime was held: {}",
                self.diagnostics()?
            );
            if Instant::now() >= deadline {
                return Ok(());
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn argv(&self) -> TestResult<Vec<String>> {
        let bytes = fs::read(self.root.path().join("argv"))?;
        bytes
            .strip_suffix(&[0])
            .ok_or("argv capture lacks trailing NUL")?
            .split(|byte| *byte == 0)
            .map(|arg| String::from_utf8(arg.to_vec()).map_err(Into::into))
            .collect()
    }

    fn diagnostics(&self) -> TestResult<String> {
        Ok(fs::read_to_string(self.root.path().join("stderr"))?)
    }
}

#[derive(Clone, Copy)]
enum Request {
    Probe,
    Release,
}

struct Control(BufReader<TcpStream>);

impl Control {
    fn new(stream: TcpStream) -> TestResult<Self> {
        stream.set_read_timeout(Some(PROCESS_TIMEOUT))?;
        stream.set_write_timeout(Some(PROCESS_TIMEOUT))?;
        Ok(Self(BufReader::new(stream)))
    }

    fn send(&mut self, request: Request) -> io::Result<()> {
        let request = match request {
            Request::Probe => "probe",
            Request::Release => "release",
        };
        writeln!(self.0.get_mut(), "{request}")
    }

    fn receive(&mut self) -> TestResult<String> {
        let mut line = String::new();
        if self.0.read_line(&mut line)? == 0 {
            return Err("runtime disconnected before acknowledgement".into());
        }
        Ok(line.trim_end().to_owned())
    }
}

struct Process {
    child: Child,
    finished: bool,
}

impl Process {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        Ok(Self {
            child: command.process_group(0).spawn()?,
            finished: false,
        })
    }

    fn finish(&mut self) -> io::Result<ExitStatus> {
        let status = wait_bounded(&mut self.child)?;
        self.finished = true;
        Ok(status)
    }

    fn cleanup(&mut self) -> io::Result<()> {
        let mut kill = Command::new("kill")
            .args(["-KILL", &format!("-{}", self.child.id())])
            .spawn()?;
        let kill_status = wait_bounded(&mut kill);
        if kill_status.is_err() {
            kill.kill()?;
            wait_bounded(&mut kill)?;
        }
        let status = kill_status?;
        if !status.success() && self.child.try_wait()?.is_none() {
            return Err(io::Error::other(format!(
                "process-group cleanup failed: {status}"
            )));
        }
        wait_bounded(&mut self.child)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if !self.finished
            && let Err(error) = self.cleanup()
            && let Err(report_error) =
                writeln!(io::stderr(), "lifecycle process cleanup failed: {error}")
        {
            tracing::error!(%error, %report_error, "could not report lifecycle cleanup failure");
        }
    }
}

fn wait_bounded(child: &mut Child) -> io::Result<ExitStatus> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "child process did not exit",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

const fn native_runtime() -> Runtime {
    if cfg!(target_os = "macos") {
        Runtime::Container
    } else {
        Runtime::Podman
    }
}

const fn runtime_name(runtime: Runtime) -> &'static str {
    match runtime {
        Runtime::Podman => "podman",
        Runtime::Container => "container",
    }
}

fn digest() -> String {
    format!("sha256:{}", "a".repeat(64))
}

#[test]
fn runtime_fixture_conforms_to_inspection_hold_release_and_status_contract() -> TestResult {
    for runtime in [Runtime::Podman, Runtime::Container] {
        let name = runtime_name(runtime);
        for exit_code in [0, 37] {
            let fixture = Fixture::new()?;
            let program = fixture.root.path().join("bin").join(name);
            let mut inspect = Command::new(&program);
            fixture.configure(&mut inspect, exit_code)?;
            let status = Process::spawn(inspect.args(["image", "inspect", IMAGE]))?.finish()?;
            assert!(status.success());
            let stdout = fs::read_to_string(fixture.root.path().join("stdout"))?;
            if runtime == Runtime::Podman {
                assert_eq!(stdout.trim(), digest());
            } else {
                let value: serde_json::Value = serde_json::from_str(&stdout)?;
                assert_eq!(value[0]["digest"], digest());
                assert_eq!(value[0]["id"], digest());
            }
            let mut command = Command::new(&program);
            fixture.configure(&mut command, exit_code)?;
            let args = ["run", "--rm", IMAGE, "", "line\nbreak", "--help"];
            let mut child = Process::spawn(command.args(args))?;
            let mut control = fixture.accept(&mut child)?;
            assert_eq!(control.receive()?, format!("ready {name}"));
            assert_eq!(fixture.argv()?, args);
            fixture.assert_held(&mut child, &mut control)?;
            control.send(Request::Release)?;
            assert_eq!(control.receive()?, format!("finished {exit_code}"));
            assert_eq!(child.finish()?.code(), Some(i32::from(exit_code)));
            assert_eq!(
                fs::read(fixture.root.path().join("stdout"))?,
                Vec::<u8>::new()
            );
        }
        let fixture = Fixture::new()?;
        let mut command = Command::new(fixture.root.path().join("bin").join(name));
        fixture.configure(&mut command, 0)?;
        let status = Process::spawn(command.args(["unexpected", "arguments"]))?.finish()?;
        assert_eq!(status.code(), Some(91));
        assert!(
            fixture
                .diagnostics()?
                .contains("unexpected runtime arguments")
        );
    }
    let fixture = Fixture::new()?;
    let mut route = Command::new(fixture.root.path().join("bin/route"));
    fixture.configure(&mut route, 0)?;
    assert!(
        Process::spawn(route.args(["-n", "get", "default"]))?
            .finish()?
            .success()
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("stdout"))?,
        "interface: en0\n"
    );
    Ok(())
}

#[test]
fn runtime_fixture_times_out_without_release() -> TestResult {
    let fixture = Fixture::new()?;
    let mut command = Command::new(fixture.root.path().join("bin/podman"));
    fixture.configure(&mut command, 0)?;
    command.env("WRIX_TEST_CONTROL_TIMEOUT", "1");
    let mut child = Process::spawn(command.args(["run", "--rm", IMAGE]))?;
    let mut control = fixture.accept(&mut child)?;
    assert_eq!(control.receive()?, "ready podman");
    assert_eq!(child.finish()?.code(), Some(92));
    assert!(fixture.diagnostics()?.contains("before release"));
    Ok(())
}

#[test]
fn process_guard_cleans_up_held_runtime_without_release() -> TestResult {
    let fixture = Fixture::new()?;
    let mut command = Command::new("bash");
    fixture.configure(&mut command, 0)?;
    command
        .args(["-c", "\"$1\" run --rm \"$2\" & wait", "--"])
        .arg(fixture.root.path().join("bin/podman"))
        .arg(IMAGE);
    let mut child = Process::spawn(&mut command)?;
    let mut control = fixture.accept(&mut child)?;
    assert_eq!(control.receive()?, "ready podman");
    fixture.assert_held(&mut child, &mut control)?;
    drop(child);
    assert_eq!(
        control
            .receive()
            .expect_err("runtime survived process-group cleanup")
            .to_string(),
        "runtime disconnected before acknowledgement"
    );
    Ok(())
}

#[test]
fn runtime_fixture_fails_when_control_disconnects_without_release() -> TestResult {
    let fixture = Fixture::new()?;
    let mut command = Command::new(fixture.root.path().join("bin/podman"));
    fixture.configure(&mut command, 0)?;
    let mut child = Process::spawn(command.args(["run", "--rm", IMAGE]))?;
    let mut control = fixture.accept(&mut child)?;
    assert_eq!(control.receive()?, "ready podman");
    drop(control);
    assert_eq!(child.finish()?.code(), Some(92));
    assert!(fixture.diagnostics()?.contains("before release"));
    Ok(())
}
