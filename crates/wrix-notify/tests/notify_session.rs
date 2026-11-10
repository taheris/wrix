#![cfg(test)]

use std::{
    env, fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const OBSERVE: &str = "WRIX_TEST_NOTIFY_SESSION_OBSERVE";
const CASE: &str = "packaged_session_preserves_exec_pid_and_owns_session_and_group";

fn launcher() -> PathBuf {
    PathBuf::from(
        env::var_os("WRIX_TEST_NOTIFY_SESSION").expect("packaged session fixture required"),
    )
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\"'\"'"))
}

fn daemon(root: &Path) {
    let shell = PathBuf::from(env::var_os("WRIX_TEST_SHELL").unwrap());
    let program = root.join("wrix-notifyd");
    fs::write(
        &program,
        format!(
            "#!{}\nexec {} --exact {CASE} --ignored --nocapture\n",
            shell.display(),
            quote(&env::current_exe().unwrap()),
        ),
    )
    .unwrap();
    fs::set_permissions(program, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
#[ignore = "requires the packaged native notification session fixture"]
fn packaged_session_preserves_exec_pid_and_owns_session_and_group() {
    if env::var_os(OBSERVE).is_some() {
        println!(
            "session:{}:{}:{}",
            std::process::id(),
            rustix::process::getsid(None).unwrap().as_raw_nonzero(),
            rustix::process::getpgrp().as_raw_nonzero(),
        );
        panic!("session observer cannot pass a semantic test");
    }
    let root = tempfile::tempdir().unwrap();
    daemon(root.path());
    let child = Command::new(launcher())
        .env("PATH", root.path())
        .env(OBSERVE, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(101), "{output:?}");
    let expected = format!("session:{pid}:{pid}:{pid}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .any(|line| line == expected),
        "the daemon must retain the launcher PID in a new session and process group"
    );
}

#[test]
#[ignore = "requires the packaged native notification session fixture"]
fn missing_daemon_fails_instead_of_claiming_readiness() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(launcher())
        .env("PATH", root.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8(output.stderr).unwrap().contains("Exec"));
    assert!(output.stdout.is_empty());
}

#[test]
#[ignore = "requires the packaged native notification session fixture"]
fn existing_group_leader_cannot_silently_skip_session_isolation() {
    let root = tempfile::tempdir().unwrap();
    daemon(root.path());
    let output = Command::new(launcher())
        .env("PATH", root.path())
        .env(OBSERVE, "1")
        .process_group(0)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Session")
    );
    assert!(output.stdout.is_empty());
}
