#[cfg(test)]
mod test {
    use std::{
        fmt::Write,
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        process::Command,
    };
    const TOOL: &str = env!("CARGO_BIN_EXE_wrix-native-test");
    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(temp.path()).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let fixture = Self { _temp: temp, root };
            fixture.file("issues-before-native-update.csv", b"issue_count\n7\n");
            fixture.file("issues-after-fixed-repair.csv", b"issue_count\n9\n");
            let mut history = String::from("commit_hash\n");
            for index in 0..10 {
                writeln!(history, "{index:032x}").unwrap();
            }
            fixture.file("history-before-native-update.csv", history.as_bytes());
            fixture
        }
        fn file(&self, name: &str, data: &[u8]) {
            let path = self.root.join(name);
            fs::write(&path, data).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        fn command(&self) -> Command {
            let mut command = Command::new(TOOL);
            command
                .args(["operator-probe", "fixed-repair", "--root"])
                .arg(&self.root);
            command
        }
        fn executable(&self, name: &str, body: &str) -> PathBuf {
            self.file(
                name,
                format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n").as_bytes(),
            );
            let path = self.root.join(name);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path
        }
    }
    #[test]
    fn dry_run_never_invokes_an_ambient_database_or_container_command() {
        let fixture = Fixture::new();
        for name in ["bd", "container"] {
            fixture.executable(name, "exit 99");
        }
        let original: std::collections::BTreeSet<_> = fs::read_dir(&fixture.root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let output = fixture
            .command()
            .env("PATH", &fixture.root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["dry_run"], true);
        assert_eq!(value["mutation"], "none");
        assert!(
            value["query"]
                .as_str()
                .unwrap()
                .starts_with("SELECT COUNT(*) AS retained")
        );
        let after: std::collections::BTreeSet<_> = fs::read_dir(&fixture.root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(original, after);
    }
    #[test]
    fn explicit_read_only_boundary_reports_actual_fixture_observations() {
        let fixture = Fixture::new();
        let bd = fixture.executable(
            "fixture-bd",
            "[[ $# == 3 && $1 == sql && $2 == --csv ]]\nprintf 'retained\\n10\\n'",
        );
        let container = fixture.executable("fixture-container", "[[ $# == 2 && $1 == inspect && $2 == wrix-host-service ]]\nprintf '%s\\n' '[{\"configuration\":{\"resources\":{\"cpus\":4,\"memoryInBytes\":1073741824}}}]'");
        let output = fixture
            .command()
            .arg("--read-only")
            .arg("--bd")
            .arg(&bd)
            .arg("--container")
            .arg(&container)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["ten_premaintenance_commits_still_reachable"], true);
        assert_eq!(receipt["retained_backend_resources"]["cpus"], 4);
    }
    #[test]
    fn invalid_commit_is_rejected_without_invoking_either_fixture_command() {
        let fixture = Fixture::new();
        fixture.file(
            "history-before-native-update.csv",
            b"commit_hash\n'; DELETE FROM dolt_log; --\n",
        );
        let output = fixture
            .command()
            .args([
                "--read-only",
                "--bd",
                "/does-not-exist",
                "--container",
                "/does-not-exist",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"");
        assert!(String::from_utf8_lossy(&output.stderr).contains("supported commit hashes"));
    }
    #[test]
    fn relative_read_only_executables_fail_before_any_snapshot_read() {
        let output = Command::new(TOOL)
            .args([
                "operator-probe",
                "fixed-repair",
                "--root",
                "/does-not-exist",
                "--read-only",
                "--bd",
                "bd",
                "--container",
                "container",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"");
        assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
    }
    #[test]
    fn strict_plan_is_explicitly_inert_and_creates_no_marker() {
        let fixture = Fixture::new();
        let marker = fixture.root.join("uncreated-marker");
        let output = Command::new(TOOL)
            .args([
                "operator-probe",
                "strict-plan",
                "--program",
                "/nix/store/00000000000000000000000000000000-native/bin/wrix-native-test",
                "--marker",
            ])
            .arg(&marker)
            .args(["--system", "aarch64-linux"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(plan["native_execution"], false);
        assert_eq!(plan["sandbox"], true);
        assert_eq!(plan["sandbox_fallback"], false);
        assert!(!marker.exists());
    }
    #[test]
    fn strict_host_requires_explicit_execution_before_any_runtime_invocation() {
        let fixture = Fixture::new();
        let output = Command::new(TOOL)
            .args(["operator-probe", "strict-host", "--container-runtime"])
            .arg(fixture.root.join("never-run"))
            .args([
                "--nix",
                "/nix/store/00000000000000000000000000000000-tools/bin/nix",
                "--program",
                "/nix/store/00000000000000000000000000000000-native/bin/wrix-native-test",
                "--system",
                "aarch64-linux",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"");
        assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
    }
    #[test]
    fn strict_worker_reports_actual_unprivileged_identity_and_refuses_existing_markers() {
        let fixture = Fixture::new();
        let marker = fixture.root.join("marker");
        let out = fixture.root.join("result");
        let invoke = |marker: &Path| {
            Command::new(TOOL)
                .args(["operator-probe", "worker", "strict"])
                .arg(marker)
                .arg(&out)
                .output()
                .unwrap()
        };
        fixture.file("marker", b"outside");
        assert!(!invoke(&marker).status.success());
        assert!(!out.exists());
        let output = invoke(&fixture.root.join("absent"));
        if rustix::process::getuid().is_root() {
            assert!(!output.status.success());
            assert!(!out.exists());
        } else {
            assert!(output.status.success());
            assert_eq!(
                fs::read_to_string(&out).unwrap(),
                format!(
                    "strict sandbox build uid={}\n",
                    rustix::process::getuid().as_raw()
                )
            );
        }
    }
}
