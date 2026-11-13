#![cfg(target_os = "linux")]
#[cfg(test)]
mod test {
    use std::{
        ffi::OsString,
        fs,
        os::unix::{
            fs::PermissionsExt,
            process::{CommandExt, ExitStatusExt},
        },
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::Mutex,
        thread,
        time::{Duration, Instant},
    };
    use wrix_native_test::native_fixture::command;
    const TOOL: &str = env!("CARGO_BIN_EXE_wrix-native-test");
    const RUNTIME: &str = env!("CARGO_BIN_EXE_wrix-interruption-fixture");
    static SERIAL: Mutex<()> = Mutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn executable(name: &str) -> PathBuf {
        std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join(name))
            .find(|path| rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok())
            .unwrap()
    }
    fn quote(path: &Path) -> String {
        format!("'{}'", path.to_str().unwrap().replace('\'', "'\"'\"'"))
    }
    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            match self.0.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => {
                    if let Err(error) = self.0.kill() {
                        eprintln!("owned fixture cancellation failed: {error}");
                    }
                    if let Err(error) = self.0.wait() {
                        eprintln!("owned fixture wait failed: {error}");
                    }
                }
                Err(error) => eprintln!("owned fixture status failed: {error}"),
            }
        }
    }
    struct Fixture {
        directory: tempfile::TempDir,
        parent: PathBuf,
        root: PathBuf,
        cleanup: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let parent = fs::canonicalize(directory.path()).unwrap();
            let root = parent.join("fixture");
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(root.join(".wrix-native-test-fixture"), "1\n").unwrap();
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
                root,
                cleanup,
            }
        }
        fn command(&self) -> Command {
            let mut command = Command::new(TOOL);
            command
                .arg("native-fixture-cleanup")
                .current_dir(&self.parent);
            command
        }
        fn supervised(&self, action: &str, timeout: &str) -> Command {
            let mut command = Command::new(executable("bash"));
            command
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run-native-test.sh"),
                )
                .arg(RUNTIME)
                .args(["interruption-runtime", "supervised-child", action])
                .env("TMPDIR", &self.parent)
                .env("WRIX_FAKE_NATIVE_ROOT", &self.root)
                .env("WRIX_TEST_POOL_CLEANUP", &self.cleanup)
                .env("WRIX_TEST_POOL_TEST_TIMEOUT", timeout)
                .env_remove("WRIX_TEST_POOL_CLEANUP_TOOLS");
            command
        }
        fn verify_runner(&self, action: &str, timeout: &str, code: i32) {
            let output = command::capture(
                &mut self.supervised(action, timeout),
                Duration::from_secs(15),
            )
            .unwrap();
            let diagnostic = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.status.code(), Some(code), "{diagnostic}");
            if code == 0 {
                assert!(!self.root.exists(), "{diagnostic}");
            } else {
                assert!(self.root.join("ready").is_file());
                let receipt: serde_json::Value = serde_json::from_slice(
                    &fs::read(self.root.join(".wrix-native-test-resources-stopped")).unwrap(),
                )
                .unwrap();
                assert_eq!(receipt["version"], 1);
                assert!(
                    diagnostic.contains("diagnostics and registry retained"),
                    "{diagnostic}"
                );
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            assert!(self.directory.path().exists());
        }
    }

    #[test]
    fn cleanup_cli_removes_only_confirmed_fixture_and_preserves_peer() {
        let _serial = serial();
        let f = Fixture::new();
        let peer = f.parent.join("peer");
        fs::create_dir(&peer).unwrap();
        let output = f
            .command()
            .arg("--remove-files")
            .arg(&f.root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!f.root.exists());
        assert!(peer.is_dir());
    }
    #[test]
    fn registry_cleanup_recovers_missing_marker_and_removes_only_completed_entry() {
        let _serial = serial();
        let f = Fixture::new();
        let registry = f.parent.join("registry");
        fs::create_dir(&registry).unwrap();
        fs::set_permissions(&registry, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(registry.join("root"), f.root.as_os_str().as_encoded_bytes()).unwrap();
        fs::remove_file(f.root.join(".wrix-native-test-fixture")).unwrap();
        let output = f
            .command()
            .arg("--registry")
            .arg(&registry)
            .arg("--remove-files")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!f.root.exists());
        assert_eq!(fs::read_dir(registry).unwrap().count(), 0);
    }
    #[test]
    fn empty_registry_cleanup_is_idempotent_and_does_not_scan_other_roots() {
        let _serial = serial();
        let f = Fixture::new();
        let registry = f.parent.join("registry");
        fs::create_dir(&registry).unwrap();
        fs::set_permissions(&registry, fs::Permissions::from_mode(0o700)).unwrap();
        for _ in 0..2 {
            assert!(
                f.command()
                    .arg("--registry")
                    .arg(&registry)
                    .arg("--remove-files")
                    .status()
                    .unwrap()
                    .success()
            );
        }
        assert!(f.root.exists());
    }
    #[test]
    fn one_failed_root_does_not_suppress_independent_cleanup_and_is_nonzero() {
        let _serial = serial();
        let f = Fixture::new();
        let other = f.parent.join("other");
        fs::create_dir(&other).unwrap();
        fs::set_permissions(&other, fs::Permissions::from_mode(0o777)).unwrap();
        let output = f
            .command()
            .arg("--remove-files")
            .arg(&other)
            .arg(&f.root)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(other.exists());
        assert!(!f.root.exists());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unsafe fixture root"));
    }
    #[test]
    fn runner_cleans_and_removes_files_on_success() {
        let _serial = serial();
        Fixture::new().verify_runner("success", "15m", 0);
    }
    #[test]
    fn runner_retains_diagnostics_after_native_assertion_failure() {
        let _serial = serial();
        Fixture::new().verify_runner("failure", "15m", 1);
    }
    #[test]
    fn runner_retains_diagnostics_after_native_process_is_killed() {
        let _serial = serial();
        Fixture::new().verify_runner("killed", "15m", 137);
    }
    #[test]
    fn runner_cleans_owned_resources_after_timeout() {
        let _serial = serial();
        Fixture::new().verify_runner("wait", "0.5s", 124);
    }
    #[test]
    fn runner_forwards_termination_only_to_its_live_direct_child() {
        let _serial = serial();
        let f = Fixture::new();
        let mut child = Owned(
            f.supervised("wait", "15m")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f.root.join("ready").exists() {
            assert!(child.0.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        let pid = rustix::process::Pid::from_raw(i32::try_from(child.0.id()).unwrap()).unwrap();
        rustix::process::kill_process_group(pid, rustix::process::Signal::TERM).unwrap();
        let status = child.0.wait().unwrap();
        let mut errors = String::new();
        std::io::Read::read_to_string(&mut child.0.stderr.take().unwrap(), &mut errors).unwrap();
        assert_eq!(status.code(), Some(143), "{errors}");
        assert!(f.root.join(".wrix-native-test-resources-stopped").is_file());
        assert!(f.root.join("ready").is_file());
    }
    #[test]
    fn pidfd_forwarding_reaches_the_original_live_child_without_unrelated_signals() {
        let _serial = serial();
        let mut child = Owned(Command::new("sleep").arg("60").spawn().unwrap());
        let mut unrelated = Owned(Command::new("sleep").arg("60").spawn().unwrap());
        let output = Command::new(TOOL)
            .args([
                OsString::from("native-fixture-cleanup"),
                "--signal-child".into(),
                child.0.id().to_string().into(),
                "--parent".into(),
                std::process::id().to_string().into(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            child.0.wait().unwrap().signal(),
            Some(rustix::process::Signal::TERM.as_raw())
        );
        assert!(unrelated.0.try_wait().unwrap().is_none());
    }
    #[test]
    fn exited_pidfd_cannot_be_redirected_to_another_process() {
        let _serial = serial();
        let mut child = Owned(Command::new("sleep").arg("60").spawn().unwrap());
        let mut unrelated = Owned(Command::new("sleep").arg("60").spawn().unwrap());
        let pid = rustix::process::Pid::from_raw(i32::try_from(child.0.id()).unwrap()).unwrap();
        let descriptor =
            rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).unwrap();
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert_eq!(
            rustix::process::pidfd_send_signal(&descriptor, rustix::process::Signal::TERM),
            Err(rustix::io::Errno::SRCH)
        );
        assert!(unrelated.0.try_wait().unwrap().is_none());
    }
    #[test]
    fn external_runtime_failure_prevents_file_deletion_and_preserves_its_diagnostic() {
        let _serial = serial();
        let f = Fixture::new();
        fs::create_dir_all(f.root.join("home/.local/share/containers/storage")).unwrap();
        let tools = f.parent.join("tools");
        fs::create_dir(&tools).unwrap();
        let podman = tools.join("podman");
        fs::write(
        &podman,
        format!(
            "#!{}\nset -euo pipefail\nprintf '%s\\n' 'fixture runtime unavailable' >&2\nexit 42\n",
            executable("bash").display()
        ),
    )
    .unwrap();
        fs::set_permissions(&podman, fs::Permissions::from_mode(0o700)).unwrap();
        let output = f
            .command()
            .args(["--remove-files", "--tools"])
            .arg(&tools)
            .arg(&f.root)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("fixture runtime unavailable"));
        assert!(f.root.exists());
        assert!(!f.root.join(".wrix-native-test-resources-stopped").exists());
    }
}
