#![cfg(test)]

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    fn process(&self, pid: u32, parent: u32, uid: u32, kernel: u32) {
        let directory = self.0.path().join(pid.to_string());
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("status"), format!("Name:\ttrusted-name-is-not-authority\nPPid:\t{parent}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nKthread:\t{kernel}\n")).unwrap();
    }

    fn invoke(&self, function: &str, pid: impl AsRef<std::ffi::OsStr>) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../lib/services/nix-pool/bootstrap-process.sh");
        Command::new("bash")
            .args([
                "-c",
                "source \"$1\"; \"$2\" \"$3\" \"$4\"",
                "bootstrap-test",
            ])
            .arg(script)
            .arg(function)
            .arg(pid)
            .arg(self.0.path())
            .env("LC_ALL", "C")
            .output()
            .unwrap()
    }

    fn chain(&self, pid: &str) -> Output {
        self.invoke("nix_pool_bootstrap_chain", pid)
    }
}

#[test]
fn kernel_classification_requires_positive_kernel_status_not_root_uid_or_name() {
    let fixture = Fixture::new();
    fixture.process(2, 0, 0, 1);
    assert!(
        fixture
            .invoke("nix_pool_bootstrap_kernel", "2")
            .status
            .success()
    );
    fixture.process(2, 0, 0, 0);
    assert!(
        !fixture
            .invoke("nix_pool_bootstrap_kernel", "2")
            .status
            .success()
    );
    fixture.process(2, 0, 1000, 1);
    assert!(
        !fixture
            .invoke("nix_pool_bootstrap_kernel", "2")
            .status
            .success()
    );
}

#[test]
fn missing_kernel_type_evidence_does_not_exempt_an_unknown_process() {
    let fixture = Fixture::new();
    fixture.process(2, 0, 0, 1);
    let status = fixture.0.path().join("2/status");
    fs::write(
        &status,
        fs::read_to_string(&status)
            .unwrap()
            .replace("Kthread:\t1\n", ""),
    )
    .unwrap();
    assert!(
        !fixture
            .invoke("nix_pool_bootstrap_kernel", "2")
            .status
            .success()
    );
}

#[test]
fn root_group_may_retain_kernel_infrastructure_but_no_unaccounted_user_process() {
    let fixture = Fixture::new();
    let cgroup = fixture.0.path().join("cgroup");
    fs::create_dir(&cgroup).unwrap();
    let members = cgroup.join("cgroup.procs");
    fixture.process(2, 0, 0, 1);
    fixture.process(42, 1, 0, 0);
    fs::write(&members, "2\n").unwrap();
    assert!(
        fixture
            .invoke("nix_pool_bootstrap_root_kernel", &cgroup)
            .status
            .success()
    );
    for value in ["2\n42\n", "0\n"] {
        fs::write(&members, value).unwrap();
        assert!(
            !fixture
                .invoke("nix_pool_bootstrap_root_kernel", &cgroup)
                .status
                .success()
        );
    }
}

#[test]
fn native_vm_init_is_admitted_only_as_a_root_startup_ancestor() {
    let fixture = Fixture::new();
    for (pid, parent) in [(1, 0), (10, 1), (11, 10), (12, 1)] {
        fixture.process(pid, parent, 0, 0);
    }
    let output = fixture.chain("11");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().collect::<Vec<_>>(), ["11", "10", "1"]);
    assert!(!stdout.lines().any(|line| line == "12"));
}

#[test]
fn direct_pid_one_bootstrap_needs_no_extra_process_exception() {
    let fixture = Fixture::new();
    fixture.process(1, 0, 0, 0);
    let output = fixture.chain("1");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"1\n");
}

#[test]
fn nonroot_ancestor_cannot_be_admitted_by_its_name_or_pid() {
    let fixture = Fixture::new();
    fixture.process(1, 0, 0, 0);
    fixture.process(10, 1, 1000, 0);
    fixture.process(11, 10, 0, 0);
    assert!(!fixture.chain("11").status.success());
}

#[test]
fn missing_and_changed_parent_are_not_assumed_to_be_native_init() {
    let fixture = Fixture::new();
    fixture.process(11, 10, 0, 0);
    assert!(!fixture.chain("11").status.success());
    fixture.process(10, 0, 0, 0);
    assert!(!fixture.chain("11").status.success());
}

#[test]
fn cycles_and_oversized_ancestry_fail_closed() {
    let fixture = Fixture::new();
    fixture.process(10, 11, 0, 0);
    fixture.process(11, 10, 0, 0);
    assert!(!fixture.chain("11").status.success());
    fixture.process(1, 0, 0, 0);
    for pid in 2..11 {
        fixture.process(pid, pid - 1, 0, 0);
    }
    assert!(fixture.chain("8").status.success());
    assert!(!fixture.chain("10").status.success());
}

#[test]
fn malformed_pid_cannot_escape_proc_or_select_process_groups() {
    let fixture = Fixture::new();
    for pid in ["0", "-1", "../1", "1/..", "01", "999999999999999999999"] {
        assert!(!fixture.chain(pid).status.success(), "{pid}");
    }
}
