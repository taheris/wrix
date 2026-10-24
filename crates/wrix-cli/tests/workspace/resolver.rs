//! Protocol fixtures are real host-selected processes and real Git administration.

use super::{Fixture, snapshot};
use std::{
    fs,
    os::unix::ffi::OsStringExt,
    process::Stdio,
    time::{Duration, Instant},
};

#[test]
fn strict_resolver_rejects_unknown_duplicate_trailing_version_and_noncanonical_fields() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let original = serde_json::json!({"version":1,"membership":"workspace-one","checkout_root_b64":wrix_state::resolver::encode(&checkout),"primary_root_b64":wrix_state::resolver::encode(&checkout)});
    let mut unknown = original.clone();
    unknown["native_key"] = "must-not-be-accepted".into();
    let mut version = original.clone();
    version["version"] = 2.into();
    let mut alias = original.clone();
    alias["primary_root_b64"] = wrix_state::resolver::encode(&checkout.join(".")).into();
    let mut padding = original.clone();
    padding["primary_root_b64"] = "L3ByaW1hcnk".into();
    let mut membership = original.clone();
    membership["membership"] = "not-the-expected-membership".into();
    for reply in [
        unknown.to_string(),
        version.to_string(),
        alias.to_string(),
        padding.to_string(),
        membership.to_string(),
        format!("{original} {original}"),
        original
            .to_string()
            .replace("\"version\":1", "\"version\":1,\"version\":1"),
    ] {
        fixture.adapter_script(&format!(
            "IFS= read -r request || [[ -n \"$request\" ]]\nprintf '%s' '{reply}'"
        ));
        let before = snapshot(&fixture.state());
        let output = fixture.admit(&checkout, "workspace-one", &["--allocate"]);
        assert!(!output.status.success(), "accepted: {reply}");
        assert_eq!(snapshot(&fixture.state()), before);
    }
}

#[test]
fn resolver_request_preserves_subdirectory_and_finishes_with_eof() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    fs::create_dir(fixture.root.path().join("checkout/child")).unwrap();
    let caller = checkout.join("child");
    let request = format!(
        r#"{{"version":1,"caller_directory_b64":"{}"}}"#,
        wrix_state::resolver::encode(&caller)
    );
    let reply = serde_json::json!({"version":1,"membership":"workspace-one","checkout_root_b64":wrix_state::resolver::encode(&checkout),"primary_root_b64":wrix_state::resolver::encode(&checkout)});
    fixture.adapter_script(&format!("IFS= read -r request || [[ -n \"$request\" ]]\n[[ \"$request\" = '{request}' ]]\nprintf '%s' '{reply}'"));
    let output = fixture.admit(&caller, "workspace-one", &["--allocate"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stdout_and_stderr_overflow_refuse_without_state_changes() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    for redirection in ["", ">&2"] {
        fixture.adapter_script(&format!(
            "IFS= read -r request || [[ -n \"$request\" ]]\nprintf '%017000d' 0 {redirection}"
        ));
        let before = snapshot(&fixture.state());
        let output = fixture.admit(&checkout, "workspace-one", &["--allocate"]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("16 KiB"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(snapshot(&fixture.state()), before);
    }
}

#[test]
fn non_utf8_checkout_paths_roundtrip_through_cli_and_protocol_without_loss() {
    let fixture = Fixture::new();
    fixture.initialize();
    fixture.repository("original");
    let name = std::ffi::OsString::from_vec(b"native-\xff".to_vec());
    match fs::rename(
        fixture.root.path().join("original"),
        fixture.root.path().join(&name),
    ) {
        Ok(()) => {}
        #[cfg(target_os = "macos")]
        Err(error) if error.raw_os_error() == Some(libc::EILSEQ) => {
            eprintln!("filesystem cannot represent non-UTF8 checkout names: {error}");
            return;
        }
        Err(error) => panic!("creating non-UTF8 checkout fixture: {error}"),
    }
    let checkout = fixture.inside("/").join(&name);
    let checkout = if fixture.isolated {
        checkout
    } else {
        fixture.root.path().join(&name)
    };
    fixture.adapter(&checkout, &checkout, "workspace-one");
    let output = fixture
        .command(&[
            "admit",
            "--integration",
            "test-manager",
            "--expect-membership",
            "workspace-one",
            "--checkout",
        ])
        .arg(&checkout)
        .arg("--allocate")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = fixture
        .command(&["inspect", "--checkout"])
        .arg(&checkout)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(wrix_core::path::WorkspaceHash::from_path(&checkout).as_str())
    );
}

#[test]
fn linked_worktree_administration_is_a_real_checkout_participant() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let commit = fixture.git(
        &checkout,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    );
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let linked = fixture.inside("linked");
    let worktree = fixture.git(
        &checkout,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    assert!(
        worktree.status.success(),
        "{}",
        String::from_utf8_lossy(&worktree.stderr)
    );
    fixture.adapter(&linked, &checkout, "workspace-one");
    let output = fixture.admit(&linked, "workspace-one", &["--allocate"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fixture
            .run(&["inspect", "--checkout", linked.to_str().unwrap()])
            .status
            .success()
    );
}

#[test]
fn descendant_held_pipes_do_not_escape_the_overall_execution_deadline() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let sleep = fixture.install("sleep");
    let marker = fixture.inside("descendant.pid");
    fixture.adapter_script(&format!("IFS= read -r request || [[ -n \"$request\" ]]\n{} 60 &\nprintf '%s' \"$!\" > '{}'\nexit 0\n", sleep.display(), marker.display()));
    let before = snapshot(&fixture.state());
    let start = Instant::now();
    let output = fixture.admit(&checkout, "workspace-one", &["--allocate"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("deadline"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(start.elapsed() >= Duration::from_secs(29));
    assert_eq!(snapshot(&fixture.state()), before);
    if cfg!(target_os = "linux") {
        let pid = fs::read_to_string(fixture.root.path().join("descendant.pid")).unwrap();
        loop {
            match fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(process) if process.split_whitespace().nth(2) == Some("Z") => break,
                Ok(_) => {
                    assert!(
                        start.elapsed() < Duration::from_secs(40),
                        "owned descendant did not terminate within the original overall bound"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    // Linux may report ESRCH when the task exits during the procfs read.
                    assert!(
                        error.kind() == std::io::ErrorKind::NotFound
                            || error.raw_os_error() == Some(libc::ESRCH),
                        "unexpected native process observation: {error}"
                    );
                    break;
                }
            }
        }
    }
    assert!(start.elapsed() < Duration::from_secs(40));
}

#[test]
fn interrupt_cleans_up_the_resolver_without_mutating_admission() {
    let fixture = Fixture::new();
    fixture.initialize();
    let checkout = fixture.repository("checkout");
    let sleep = fixture.install("sleep");
    let marker = fixture.inside("ready");
    fixture.adapter_script(&format!(
        "IFS= read -r request || [[ -n \"$request\" ]]\nprintf '%s' \"$$\" > '{}'\nexec {} 60\n",
        marker.display(),
        sleep.display()
    ));
    let before = snapshot(&fixture.state());
    let mut child = fixture
        .command(&[
            "admit",
            "--integration",
            "test-manager",
            "--expect-membership",
            "workspace-one",
            "--checkout",
            checkout.to_str().unwrap(),
            "--allocate",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let ready = fixture.root.path().join("ready");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists());
    let start = Instant::now();
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
    let deadline = Instant::now() + Duration::from_secs(7);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("interrupted"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(start.elapsed() < Duration::from_secs(6));
    assert_eq!(snapshot(&fixture.state()), before);
}
