//! The parent owns disposable native resources even if its assertion subprocess panics.
use super::Fixture;
use std::{fs, ops::Deref, path::PathBuf, process::Command};

pub(super) struct NativeFixture {
    fixture: Fixture,
    finished: bool,
    registry_entry: Option<PathBuf>,
    python: std::ffi::OsString,
}

impl NativeFixture {
    pub(super) fn new(mut fixture: Fixture) -> Self {
        assert!(!fixture.isolated, "native fixtures require a real host");
        fixture.root.disable_cleanup(true);
        fs::write(
            fixture.root.path().join(".wrix-native-test-fixture"),
            b"1\n",
        )
        .unwrap();
        let registry_entry = std::env::var_os("WRIX_TEST_POOL_FIXTURE_REGISTRY").map(|registry| {
            let entry = PathBuf::from(registry).join(fixture.root.path().file_name().unwrap());
            fs::write(&entry, fixture.root.path().as_os_str().as_encoded_bytes()).unwrap();
            entry
        });
        let python = std::env::var_os("WRIX_TEST_POOL_PYTHON").unwrap_or_else(|| "python3".into());
        Self {
            fixture,
            finished: false,
            registry_entry,
            python,
        }
    }

    fn cleanup(&self, remove_files: bool) -> Result<(), String> {
        let mut command = Command::new("timeout");
        command
            .args(["--kill-after=5s", "120s"])
            .arg(&self.python)
            .arg("-c")
            .arg(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/store/cleanup-native-fixtures.py"
            )))
            .arg(self.fixture.root.path());
        if remove_files {
            command.arg("--remove-files");
        } else {
            command.arg("--diagnostics");
        }
        if let Some(path) = std::env::var_os("WRIX_NIX_POOL_HOST_TOOLS") {
            let bytes = fs::read(path).map_err(|error| error.to_string())?;
            let tools: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            let root = tools["root"]
                .as_str()
                .ok_or("missing package-selected cleanup tools")?;
            command.arg("--tools").arg(PathBuf::from(root).join("bin"));
        }
        let output = command.output().map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "{}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        if !output.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        if remove_files && let Some(entry) = &self.registry_entry {
            fs::remove_file(entry).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) {
        let result = self.cleanup(true);
        self.finished = true;
        match result {
            Ok(()) => self.fixture.root.disable_cleanup(false),
            Err(error) => panic!(
                "native fixture cleanup failed; retained {:?}: {error}",
                self.fixture.root.path()
            ),
        }
    }
}

impl Deref for NativeFixture {
    type Target = Fixture;
    fn deref(&self) -> &Self::Target {
        &self.fixture
    }
}

impl Drop for NativeFixture {
    fn drop(&mut self) {
        if !self.finished {
            match self.cleanup(false) {
                Ok(()) => eprintln!(
                    "native failure resources stopped; diagnostics retained {:?}",
                    self.fixture.root.path()
                ),
                Err(error) => eprintln!(
                    "native fixture cleanup failed; retained {:?}: {error}",
                    self.fixture.root.path()
                ),
            }
        }
    }
}

#[test]
#[cfg(target_os = "linux")]
fn failed_native_probe_terminates_its_helper_process_group() {
    use std::{
        io::{BufRead, BufReader, Read},
        os::unix::process::CommandExt,
        process::Stdio,
        sync::mpsc,
        time::Duration,
    };
    let mut command = Command::new("python3");
    command.args(["-c", "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c','import time; time.sleep(4)']); print('ready',flush=True); time.sleep(60)"]);
    command.stdout(Stdio::piped()).process_group(0);
    let mut running = super::Running(command.spawn().unwrap());
    let mut output = BufReader::new(running.0.stdout.take().unwrap());
    let mut ready = String::new();
    output.read_line(&mut ready).unwrap();
    assert_eq!(ready, "ready\n");
    drop(running);
    let stopped = std::thread::scope(|scope| {
        let (done_tx, done) = mpsc::channel();
        let reader = scope.spawn(move || {
            let mut rest = String::new();
            output.read_to_string(&mut rest).unwrap();
            done_tx.send(()).unwrap();
        });
        let stopped = done.recv_timeout(Duration::from_secs(2)).is_ok();
        reader.join().unwrap();
        stopped
    });
    assert!(
        stopped,
        "native helper still holds a descriptor after its assertion process was killed"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn failed_parent_cleanup_retains_the_fixture_instead_of_hiding_live_resources() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().to_owned();
    let mut fixture = NativeFixture::new(Fixture {
        root,
        isolated: false,
    });
    fixture.python = path.join("missing-cleanup-interpreter").into_os_string();
    let result = std::panic::catch_unwind(|| fixture.finish());
    assert!(result.is_err());
    assert!(path.join(".wrix-native-test-fixture").is_file());
    fs::remove_dir_all(path).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn parent_guard_stops_resources_and_retains_diagnostics_after_assertion_unwind() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().to_owned();
    let result = std::panic::catch_unwind(|| {
        let _fixture = NativeFixture::new(Fixture {
            root,
            isolated: false,
        });
        panic!("failed native assertion");
    });
    assert!(result.is_err());
    assert!(path.join(".wrix-native-test-resources-stopped").is_file());
    fs::remove_dir_all(path).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn parent_guard_checks_cleanup_before_successful_fixture_removal() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().to_owned();
    NativeFixture::new(Fixture {
        root,
        isolated: false,
    })
    .finish();
    assert!(!path.exists());
}
