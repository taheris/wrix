//! Real screenshot shell cleanup after an injected external Nix build failure.
#![cfg(test)]
use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{Read, Seek, SeekFrom, Write},
    net::{SocketAddrV4, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\"'\"'"))
}
fn missing(pid: u32) -> bool {
    match test_kill_process(Pid::from_raw(i32::try_from(pid).unwrap()).unwrap()) {
        Err(rustix::io::Errno::SRCH) => true,
        Ok(()) => false,
        Err(error) => panic!("cannot observe owned HTTP child: {error}"),
    }
}
fn evidence(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
struct ChildGroup(Child);
impl Drop for ChildGroup {
    fn drop(&mut self) {
        if self.0.try_wait().expect("observe owned verifier").is_none() {
            match kill_process_group(
                Pid::from_raw(i32::try_from(self.0.id()).unwrap()).unwrap(),
                Signal::KILL,
            ) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => panic!("cannot terminate owned verifier group: {error}"),
            }
            self.0.wait().expect("reap owned verifier");
        }
    }
}
struct ServerCleanup(PathBuf);
impl Drop for ServerCleanup {
    fn drop(&mut self) {
        if self.0.exists() {
            let value = evidence(&self.0).unwrap();
            let pid = u32::try_from(value["pid"].as_u64().unwrap()).unwrap();
            if !missing(pid) {
                match kill_process(
                    Pid::from_raw(i32::try_from(pid).unwrap()).unwrap(),
                    Signal::TERM,
                ) {
                    Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                    Err(error) => panic!("cannot retire owned HTTP fixture: {error}"),
                }
            }
        }
    }
}

#[test]
fn fixture_executable_quoting_preserves_shell_metacharacters() -> Result {
    let path = Path::new("/private/tmp/a b'\"$`雪\nfixture");
    let output = Command::new("bash")
        .args(["-c", &format!("printf '%s' {}", quote(path))])
        .output()?;
    assert!(output.status.success());
    assert_eq!(output.stdout, path.to_str().unwrap().as_bytes());
    Ok(())
}

#[test]
fn package_failure_fixture_rejects_unknown_operations_before_claiming_http_evidence() -> Result {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("nix-args"), b"unexpected\0")?;
    let output = Command::new(env::current_exe()?)
        .args(["--ignored", "--exact", "fail_package_build"])
        .env("CLEANUP_FIXTURE_ROOT", root.path())
        .output()?;
    assert!(!output.status.success());
    assert_ne!(output.status.code(), Some(42));
    assert!(!root.path().join("evidence.json").exists());
    Ok(())
}

#[test]
#[ignore = "external Nix mock subprocess, not a standalone scenario"]
fn fail_package_build() -> Result {
    let root = PathBuf::from(env::var_os("CLEANUP_FIXTURE_ROOT").ok_or("missing fixture root")?);
    let bytes = fs::read(root.join("nix-args"))?;
    let args = bytes
        .strip_suffix(&[0])
        .ok_or("argument terminator missing")?
        .split(|b| *b == 0)
        .map(|v| String::from_utf8(v.to_vec()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(args[0], "build");
    let index = args
        .iter()
        .position(|arg| arg == "--argstr")
        .ok_or("missing repo argument")?
        + 1;
    assert_eq!(
        &args[index..index + 2],
        &["repoRoot", &env::var("PLAYWRIGHT_REPO_ROOT")?]
    );
    assert_eq!(&args[args.len() - 2..], &["packageName", "playwright-mcp"]);
    let temp = PathBuf::from(env::var_os("TMPDIR").ok_or("missing TMPDIR")?);
    let ports = fs::read_dir(&temp)?
        .map(|entry| entry.map(|entry| entry.path().join("http_port")))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert_eq!(ports.len(), 1);
    let port: u16 = fs::read_to_string(&ports[0])?.trim().parse()?;
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse()?,
        Duration::from_secs(5),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut response = String::new();
    stream.take(1024 * 1024).read_to_string(&mut response)?;
    assert!(response.starts_with("HTTP/1.0 200 ") || response.starts_with("HTTP/1.1 200 "));
    assert!(response.contains("Playwright MCP Screenshot Test"));
    let directory = ports[0].parent().ok_or("missing server directory")?;
    let server = directory.join("server.py").display().to_string();
    let processes = Command::new("ps")
        .args(["-eo", "pid=,ppid=,args="])
        .output()?;
    assert!(processes.status.success());
    let processes = String::from_utf8(processes.stdout)?;
    let rows = processes
        .lines()
        .filter(|row| row.contains(&server))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    let fields = rows[0].split_whitespace().collect::<Vec<_>>();
    let pid: u32 = fields[0].parse()?;
    let parent: u32 = fields[1].parse()?;
    fs::write(
        root.join("evidence.json"),
        serde_json::to_vec(&json!({"pid":pid,"ppid":parent,"port":port,"temp_dir":directory}))?,
    )?;
    eprintln!("Injected Playwright package build failure");
    std::process::exit(42)
}

#[test]
#[ignore = "owning screenshot shell boundary"]
fn setup_failure_terminates_and_reaps_http_child() -> Result {
    use std::os::unix::{fs::PermissionsExt, process::CommandExt};
    let script = PathBuf::from(
        env::var_os("WRIX_TEST_SCREENSHOT_SCRIPT").ok_or("missing screenshot script")?,
    )
    .canonicalize()?;
    let root = tempfile::tempdir()?;
    let temp = root.path().join("tmp");
    let bin = root.path().join("bin");
    fs::create_dir(&temp)?;
    fs::create_dir(&bin)?;
    let evidence_path = root.path().join("evidence.json");
    let _server_cleanup = ServerCleanup(evidence_path.clone());
    let wrapper = bin.join("nix");
    fs::write(
        &wrapper,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\0' \"$@\" >\"$CLEANUP_FIXTURE_ROOT/nix-args\"\nexec {} --ignored --exact fail_package_build --nocapture\n",
            quote(&env::current_exe()?)
        ),
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;
    let stdout = tempfile::tempfile()?;
    let stderr = stdout.try_clone()?;
    let path = env::join_paths(std::iter::once(bin).chain(env::split_paths(
        &env::var_os("PATH").ok_or("missing PATH")?,
    )))?;
    let mut child = ChildGroup(
        Command::new("bash")
            .arg(&script)
            .arg("test_screenshot_returns_png")
            .env("PATH", path)
            .env("TMPDIR", &temp)
            .env("CLEANUP_FIXTURE_ROOT", root.path())
            .env("PLAYWRIGHT_SYSTEM", "x86_64-linux")
            .env(
                "PLAYWRIGHT_REPO_ROOT",
                script.ancestors().nth(4).ok_or("missing repository root")?,
            )
            .env("PLAYWRIGHT_SERVER_CONFIG_FILE", "")
            .env("WRIX_MCP_MANIFEST", "")
            .process_group(0)
            .stdout(Stdio::from(stdout.try_clone()?))
            .stderr(Stdio::from(stderr))
            .spawn()?,
    );
    let parent = child.0.id();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            return Err("screenshot verifier exceeded original twenty-second deadline".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = stdout;
    stdout.seek(SeekFrom::Start(0))?;
    let mut output = String::new();
    stdout.take(1024 * 1024 + 1).read_to_string(&mut output)?;
    assert!(
        output.len() <= 1024 * 1024,
        "verifier output exceeded one MiB"
    );
    assert!(evidence_path.is_file(), "{output}");
    let state = evidence(&evidence_path)?;
    assert_eq!(status.code(), Some(1), "{output}");
    assert!(
        output.contains("Injected Playwright package build failure"),
        "{output}"
    );
    assert_eq!(
        state["ppid"], parent,
        "HTTP PID must belong to the verifier caller"
    );
    let pid = u32::try_from(state["pid"].as_u64().ok_or("missing PID")?)?;
    assert!(missing(pid), "HTTP child is still alive or unreaped");
    let port = u16::try_from(state["port"].as_u64().ok_or("missing port")?)?;
    let socket = rustix::net::socket(
        rustix::net::AddressFamily::INET,
        rustix::net::SocketType::STREAM,
        None,
    )?;
    rustix::net::sockopt::set_socket_reuseaddr(&socket, true)?;
    rustix::net::bind(
        &socket,
        &SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, port),
    )?;
    assert!(!Path::new(state["temp_dir"].as_str().ok_or("missing directory")?).exists());
    assert_eq!(fs::read_dir(&temp)?.count(), 0);
    Ok(())
}
