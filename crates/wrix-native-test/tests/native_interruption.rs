#![cfg(target_os = "linux")]

#[cfg(test)]
mod test {
    use serde_json::Value;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::Mutex,
        time::Duration,
    };
    use wrix_native_test::native_fixture::command;

    static SERIAL: Mutex<()> = Mutex::new(());
    const TOOL: &str = env!("CARGO_BIN_EXE_wrix-native-test");
    const RUNTIME: &str = env!("CARGO_BIN_EXE_wrix-interruption-fixture");

    fn executable(name: &str) -> PathBuf {
        std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join(name))
            .find(|path| rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok())
            .unwrap()
    }
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn quote(path: &Path) -> String {
        format!("'{}'", path.to_str().unwrap().replace('\'', "'\"'\"'"))
    }
    struct Fixture {
        directory: tempfile::TempDir,
        parent: PathBuf,
        bin: PathBuf,
        cleanup: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let parent = fs::canonicalize(directory.path()).unwrap();
            let bin = parent.join("bin");
            fs::create_dir(&bin).unwrap();
            for name in ["native", "podman", "cat", "readlink", "fusermount3", "git"] {
                symlink(RUNTIME, bin.join(name)).unwrap();
            }
            let cleanup = parent.join("cleanup");
            fs::write(
                &cleanup,
                format!(
                    "#!{}\nset -euo pipefail\nexec {} native-fixture-cleanup \"$@\"\n",
                    executable("bash").display(),
                    quote(Path::new(TOOL))
                ),
            )
            .unwrap();
            fs::set_permissions(&cleanup, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                directory,
                parent,
                bin,
                cleanup,
            }
        }
        fn root(&self, scenario: &str) -> PathBuf {
            self.parent.join(scenario).join("fixture")
        }
        fn command(&self, scenario: &str) -> Command {
            let parent = self.parent.join(scenario);
            fs::create_dir(&parent).unwrap();
            let mut invocation = Command::new(TOOL);
            let path = std::env::join_paths(
                std::iter::once(self.bin.clone())
                    .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
            )
            .unwrap();
            invocation
                .args(["native-interruption", "--tools"])
                .arg(&self.bin)
                .arg("--source-revision")
                .arg("a".repeat(40))
                .arg("--output")
                .arg(parent.join("evidence"))
                .arg("--ready-timeout")
                .arg(if scenario == "timeout" { "2" } else { "8" })
                .args(["--stop-timeout", "10", "--"])
                .arg(executable("bash"))
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run-native-test.sh"),
                )
                .arg(self.bin.join("native"))
                .env("PATH", path)
                .env("WRIX_FAKE_NATIVE_ROOT", self.root(scenario))
                .env("WRIX_FAKE_SCENARIO", scenario)
                .env("WRIX_TEST_POOL_CLEANUP", &self.cleanup)
                .env("WRIX_TEST_POOL_CLEANUP_TOOLS", &self.bin);
            invocation
        }
        fn run(&self, scenario: &str, success: bool) -> (Value, String) {
            let output =
                command::capture(&mut self.command(scenario), Duration::from_secs(45)).unwrap();
            let path = self.parent.join(scenario).join("evidence");
            let log = fs::read_to_string(path.join("native.log")).unwrap();
            let diagnostic = format!(
                "{}{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                log
            );
            assert_eq!(output.status.success(), success, "{scenario}: {diagnostic}");
            let result: Value =
                serde_json::from_slice(&fs::read(path.join("result.json")).unwrap()).unwrap();
            assert_eq!(
                result["outcome"],
                if success { "passed" } else { "failed" },
                "{diagnostic}"
            );
            assert_eq!(
                fs::read_to_string(path.join("exit")).unwrap(),
                if success { "0\n" } else { "1\n" }
            );
            assert!(path.join("registry/root").is_file());
            assert!(self.root(scenario).is_dir());
            (result, diagnostic)
        }
        fn json(&self, scenario: &str, file: &str) -> Value {
            serde_json::from_slice(
                &fs::read(self.parent.join(scenario).join("evidence").join(file)).unwrap(),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            assert!(self.directory.path().exists());
        }
    }

    #[test]
    fn interruption_pins_original_namespace_and_verifies_all_resources_after_signal() {
        let _serial = serial();
        let f = Fixture::new();
        let (result, output) = f.run("pass", true);
        assert_eq!(result["native_exit"], 143);
        assert_eq!(result["revision"], "a".repeat(40));
        let before = f.json("pass", "namespace-before.json");
        let after = f.json("pass", "namespace-after.json");
        assert_eq!(before["namespace"], after["namespace"]);
        assert_eq!(before["running"].as_array().unwrap().len(), 3);
        assert_eq!(after["containers"], serde_json::json!([]));
        assert_eq!(after["mountinfo"], "");
        assert!(output.contains("Verified native readiness"));
        assert!(output.contains("PASS:"));
        assert!(
            f.root("pass")
                .join(".wrix-native-test-resources-stopped")
                .is_file()
        );
        assert_eq!(result["resource_inventory"]["verified"], true);
    }
    #[test]
    fn early_native_failure_and_unexpected_success_do_not_count_as_interruption() {
        let _serial = serial();
        for (scenario, code) in [("early-exit", 7), ("early-success", 0)] {
            let f = Fixture::new();
            let (result, _) = f.run(scenario, false);
            assert_eq!(result["native_exit"], code);
            assert!(
                result["error"]
                    .as_str()
                    .unwrap()
                    .contains("before verified readiness")
            );
        }
    }
    #[test]
    fn foreign_progress_or_readiness_never_authorizes_signal() {
        let _serial = serial();
        for scenario in ["foreign", "foreign-progress"] {
            let f = Fixture::new();
            let (result, _) = f.run(scenario, false);
            assert!(result["error"].as_str().unwrap().contains("foreign"));
            assert!(result.get("signal").is_none());
        }
    }
    #[test]
    fn world_readable_readiness_is_not_authority() {
        let _serial = serial();
        let f = Fixture::new();
        let (result, _) = f.run("unsafe-ready", false);
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("unsafe native readiness")
        );
        assert!(result.get("signal").is_none());
    }
    #[test]
    fn empty_native_fixture_cannot_claim_worker_readiness() {
        let _serial = serial();
        let f = Fixture::new();
        let (result, _) = f.run("empty-ready", false);
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("not all running")
        );
        assert!(result.get("signal").is_none());
    }
    #[test]
    fn cleanup_failure_overrides_signal_exit_and_retains_final_inventory() {
        let _serial = serial();
        let f = Fixture::new();
        let (result, _) = f.run("cleanup-failure", false);
        assert_eq!(result["native_exit"], 1);
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("expected intentional SIGTERM")
        );
        assert!(
            !f.json("cleanup-failure", "namespace-after.json")["containers"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(result["resource_inventory"]["verified"], false);
        assert!(
            f.parent
                .join("cleanup-failure/evidence/host-after.mountinfo")
                .is_file()
        );
    }
    #[test]
    fn resistant_helper_retains_actual_kernel_identity_without_a_pass() {
        let _serial = serial();
        let f = Fixture::new();
        let (result, _) = f.run("term-resistant-helper", false);
        assert_eq!(result["native_exit"], 143);
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("did not exit gracefully")
        );
        let pid: u32 = fs::read_to_string(f.root("term-resistant-helper").join("escaped-pid"))
            .unwrap()
            .parse()
            .unwrap();
        let records = f.json("term-resistant-helper", "helpers-after-term.json");
        let helper = records
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["pid"] == pid)
            .unwrap();
        assert_eq!(helper["session"], result["supervisor_pid"]);
        assert!(
            helper["cmdline"]
                .as_array()
                .unwrap()
                .iter()
                .any(|word| word == "SIG_IGN")
        );
        assert!(helper["exe"].is_string());
        assert!(helper.get("wchan").is_some());
        assert!(!helper["threads"].as_array().unwrap().is_empty());
        let ignored = helper["status"]
            .as_str()
            .unwrap()
            .lines()
            .find(|line| line.starts_with("SigIgn:"))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap();
        assert_ne!(
            u64::from_str_radix(ignored, 16).unwrap()
                & (1 << (rustix::process::Signal::TERM.as_raw() - 1)),
            0
        );
        assert_eq!(result["resource_inventory"]["verified"], true);
        let stat = PathBuf::from(format!("/proc/{pid}/stat"));
        if stat.exists() {
            assert_eq!(
                wrix_native_test::native_fixture::process::Record::parse(&fs::read(stat).unwrap())
                    .unwrap()
                    .state,
                b'Z'
            );
        }
    }
    #[test]
    fn failed_namespace_inspection_keeps_partial_reply_and_fails_closed() {
        let _serial = serial();
        let f = Fixture::new();
        let (result, _) = f.run("inspection-failure", false);
        let after = f.json("inspection-failure", "namespace-after.json");
        assert_eq!(after["namespace"], "mnt:[1]");
        assert_eq!(after["containers"], serde_json::json!([]));
        assert!(after.get("error").is_some());
        assert!(after.get("images").is_none());
        assert_eq!(result["resource_inventory"]["verified"], false);
        assert_eq!(result["native_exit"], 143);
    }
    #[test]
    fn replacement_empty_namespace_cannot_prove_absence() {
        let _serial = serial();
        let f = Fixture::new();
        let (result, _) = f.run("changed-namespace", false);
        assert_eq!(result["native_exit"], 143);
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("namespace changed")
        );
    }
    #[test]
    fn teardown_receipt_does_not_replace_fresh_export_and_container_inventory() {
        let _serial = serial();
        for (scenario, message) in [
            ("post-mount", "namespace mounts"),
            ("post-container", "still owns containers"),
        ] {
            let f = Fixture::new();
            let (result, _) = f.run(scenario, false);
            assert_eq!(result["native_exit"], 143);
            assert!(result["error"].as_str().unwrap().contains(message));
            assert!(
                f.root(scenario)
                    .join(".wrix-native-test-resources-stopped")
                    .is_file()
            );
        }
    }
    #[test]
    fn hung_cleanup_is_bounded_and_never_counts_as_native_acceptance() {
        let _serial = serial();
        let f = Fixture::new();
        let (result, _) = f.run("hung-cleanup", false);
        assert_eq!(result["native_exit"], -9);
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("graceful native teardown exceeded")
        );
        assert!(
            result["supervisor_cleanup_error"]
                .as_str()
                .unwrap()
                .contains("deadline")
        );
    }
    #[test]
    fn readiness_timeout_keeps_setup_phase_and_teardown_without_signal_coverage() {
        let _serial = serial();
        let f = Fixture::new();
        let (result, output) = f.run("timeout", false);
        assert!(result.get("signal").is_none());
        assert!(result["error"].as_str().unwrap().contains("deadline"));
        assert!(output.contains("Waiting for pool"));
        assert!(output.contains("Native setup: worker-image"));
        assert_eq!(result["setup_phase"], "worker-image");
        assert!(
            f.root("timeout")
                .join(".wrix-native-test-resources-stopped")
                .is_file()
        );
    }
    #[test]
    fn escaped_helper_is_reaped_without_signalling_unrelated_process() {
        let _serial = serial();
        let mut unrelated = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let f = Fixture::new();
        let (result, output) = f.run("escaped-helper", true);
        assert!(
            !result["assertion_helpers_terminated"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(output.contains("assertion helpers"));
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
        let pid: u32 = fs::read_to_string(f.root("escaped-helper").join("escaped-pid"))
            .unwrap()
            .parse()
            .unwrap();
        let stat = PathBuf::from(format!("/proc/{pid}/stat"));
        if stat.exists() {
            assert_eq!(
                wrix_native_test::native_fixture::process::Record::parse(&fs::read(stat).unwrap())
                    .unwrap()
                    .state,
                b'Z'
            );
        }
    }
    #[test]
    fn registry_alias_is_rejected_before_any_native_fixture_starts() {
        let _serial = serial();
        let f = Fixture::new();
        let target = f.parent.join("private");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = f.parent.join("alias");
        symlink(&target, &alias).unwrap();
        let root = f.parent.join("fixture");
        let output = Command::new(executable("bash"))
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run-native-test.sh"))
            .arg("--registry")
            .arg(alias)
            .arg(f.bin.join("native"))
            .env("WRIX_TEST_POOL_CLEANUP", &f.cleanup)
            .env("WRIX_FAKE_NATIVE_ROOT", &root)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!root.exists());
    }
}
