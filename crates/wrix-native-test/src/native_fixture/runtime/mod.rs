//! External process and runtime stubs, never evidence of native containment.
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// unsupported interruption fixture invocation
    Usage,
    /// missing interruption fixture environment: {0}
    Environment(&'static str),
    /// interruption fixture filesystem or process failed: {0}
    Io(#[from] io::Error),
    /// interruption fixture state failed: {0}
    Json(#[from] serde_json::Error),
    /// interruption helper did not become ready
    Readiness,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum View {
    C,
    D,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scenario {
    Pass,
    EarlyExit,
    EarlySuccess,
    EmptyReady,
    ForeignProgress,
    TermResistantHelper,
    Timeout,
    Foreign,
    UnsafeReady,
    EscapedHelper,
    HungSupervisor,
    ChangedNamespace,
    PostMount,
    PostContainer,
    InspectionFailure,
    HungCleanup,
    CleanupFailure,
}
impl Scenario {
    fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "pass" => Ok(Self::Pass),
            "early-exit" => Ok(Self::EarlyExit),
            "early-success" => Ok(Self::EarlySuccess),
            "empty-ready" => Ok(Self::EmptyReady),
            "foreign-progress" => Ok(Self::ForeignProgress),
            "term-resistant-helper" => Ok(Self::TermResistantHelper),
            "timeout" => Ok(Self::Timeout),
            "foreign" => Ok(Self::Foreign),
            "unsafe-ready" => Ok(Self::UnsafeReady),
            "escaped-helper" => Ok(Self::EscapedHelper),
            "hung-supervisor" => Ok(Self::HungSupervisor),
            "changed-namespace" => Ok(Self::ChangedNamespace),
            "post-mount" => Ok(Self::PostMount),
            "post-container" => Ok(Self::PostContainer),
            "inspection-failure" => Ok(Self::InspectionFailure),
            "hung-cleanup" => Ok(Self::HungCleanup),
            "cleanup-failure" => Ok(Self::CleanupFailure),
            _ => Err(Error::Usage),
        }
    }
}
impl View {
    fn name(self) -> String {
        match self {
            Self::C => "c",
            Self::D => "d",
        }
        .repeat(64)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "The fixture wire payload independently observes processes, containers, images and migration; coupling them would hide partial teardown."
)]
struct State {
    running: bool,
    containers: bool,
    images: bool,
    views: Vec<View>,
    migrated: bool,
}
impl State {
    fn initial(empty: bool) -> Self {
        Self {
            running: !empty,
            containers: !empty,
            images: !empty,
            views: if empty {
                vec![]
            } else {
                vec![View::C, View::D]
            },
            migrated: false,
        }
    }
}

fn environment(key: &'static str) -> Result<OsString, Error> {
    std::env::var_os(key).ok_or(Error::Environment(key))
}
fn save(root: &Path, state: &State) -> Result<(), Error> {
    fs::write(root.join("fake-state.json"), serde_json::to_vec(state)?)?;
    Ok(())
}
fn load(root: &Path) -> Result<State, Error> {
    Ok(serde_json::from_slice(&fs::read(
        root.join("fake-state.json"),
    )?)?)
}
fn sleep_forever() -> ! {
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}
fn record(
    root: &Path,
    phase: &str,
    filename: &str,
    foreign: bool,
    public: bool,
    nonce: &str,
) -> Result<(), Error> {
    let metadata = root.metadata()?;
    let nonce = if foreign {
        "e".repeat(64)
    } else {
        nonce.to_owned()
    };
    let value = serde_json::json!({"version":1,"phase":phase,"nonce":nonce,"device":metadata.dev(),"inode":metadata.ino()});
    let pending = root.join("checkpoint.pending");
    fs::write(&pending, serde_json::to_vec(&value)?)?;
    fs::set_permissions(
        &pending,
        fs::Permissions::from_mode(if public { 0o644 } else { 0o600 }),
    )?;
    fs::rename(pending, root.join(filename))?;
    Ok(())
}
fn ignored_command(command: &mut Command) {
    command.args(["-c", "trap '' TERM; exec \"$@\"", "interruption-helper"]);
}
fn helper(root: &Path, resistant: bool) -> Result<(), Error> {
    let executable = std::env::current_exe()?;
    let ready = root.join("helper-ready");
    let mut command = if resistant {
        let mut command = Command::new("bash");
        ignored_command(&mut command);
        command.arg(&executable);
        command
    } else {
        Command::new(&executable)
    };
    command.args(["interruption-runtime", "helper"]);
    if resistant {
        command.arg("SIG_IGN").arg(&ready);
    }
    command.process_group(0);
    let mut child = command.spawn()?;
    fs::write(root.join("escaped-pid"), child.id().to_string())?;
    if resistant {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            if child.try_wait()?.is_some() || Instant::now() >= deadline {
                return Err(Error::Readiness);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}
fn native(root: &Path, scenario: Scenario, ignored: bool) -> Result<ExitCode, Error> {
    if scenario == Scenario::HungSupervisor && !ignored {
        let mut command = Command::new("bash");
        ignored_command(&mut command);
        command
            .arg(std::env::current_exe()?)
            .args(["interruption-runtime", "native-ignored"]);
        return Err(command.exec().into());
    }
    fs::create_dir(root)?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    fs::create_dir_all(root.join("home/.local/share/containers/storage"))?;
    fs::create_dir(root.join("runtime"))?;
    fs::set_permissions(root.join("runtime"), fs::Permissions::from_mode(0o700))?;
    fs::write(root.join(".wrix-native-test-fixture"), "1\n")?;
    fs::write(
        PathBuf::from(environment("WRIX_TEST_POOL_FIXTURE_REGISTRY")?).join("root"),
        root.as_os_str().as_encoded_bytes(),
    )?;
    save(root, &State::initial(scenario == Scenario::EmptyReady))?;
    if scenario == Scenario::EarlyExit {
        return Ok(ExitCode::from(7));
    }
    if scenario == Scenario::EarlySuccess {
        return Ok(ExitCode::SUCCESS);
    }
    let nonce = environment("WRIX_TEST_POOL_INTERRUPT_NONCE")?
        .into_string()
        .map_err(|_| Error::Usage)?;
    record(
        root,
        "worker-image",
        ".wrix-native-test-progress.json",
        scenario == Scenario::ForeignProgress,
        false,
        &nonce,
    )?;
    if scenario == Scenario::TermResistantHelper {
        helper(root, true)?;
    }
    if scenario != Scenario::Timeout {
        record(
            root,
            "workers-ready",
            ".wrix-native-test-ready.json",
            scenario == Scenario::Foreign,
            scenario == Scenario::UnsafeReady,
            &nonce,
        )?;
    }
    if scenario == Scenario::EscapedHelper {
        helper(root, false)?;
    }
    sleep_forever()
}
fn podman(root: &Path, scenario: Scenario, args: &[String]) -> Result<ExitCode, Error> {
    let prefix = [
        "--remote=false".to_owned(),
        "--root".into(),
        root.join("home/.local/share/containers/storage")
            .display()
            .to_string(),
        "--runroot".into(),
        root.join("runtime/containers").display().to_string(),
        "--tmpdir".into(),
        root.join("runtime/libpod/tmp").display().to_string(),
    ];
    let tail = args.strip_prefix(prefix.as_slice()).ok_or(Error::Usage)?;
    let (operation, tail) = tail.split_first().ok_or(Error::Usage)?;
    if operation == "unshare" {
        let (program, arguments) = tail.split_first().ok_or(Error::Usage)?;
        if matches!(
            Path::new(program).file_name().and_then(|s| s.to_str()),
            Some("chown" | "chmod")
        ) {
            return Ok(ExitCode::SUCCESS);
        }
        return Err(Command::new(program).args(arguments).exec().into());
    }
    let mut state = load(root)?;
    match operation.as_str() {
        "ps" => {
            if tail != ["--no-trunc", "--format", "{{.ID}}"]
                && tail != ["--all", "--no-trunc", "--format", "{{.ID}}"]
            {
                return Err(Error::Usage);
            }
            if if tail.first().is_some_and(|s| s == "--all") {
                state.containers
            } else {
                state.running
            } || scenario == Scenario::PostContainer && state.migrated
            {
                for digit in ["1", "2", "3"] {
                    writeln!(io::stdout().lock(), "{}", digit.repeat(64))?;
                }
            }
        }
        "images" => {
            if scenario == Scenario::InspectionFailure && state.migrated {
                return Ok(ExitCode::FAILURE);
            }
            if tail != ["--no-trunc", "--format", "{{.ID}}"] {
                return Err(Error::Usage);
            }
            if state.images {
                writeln!(io::stdout().lock(), "sha256:{}", "4".repeat(64))?;
            }
        }
        "inspect"
            if tail.len() == 3
                && tail[..2] == ["--format", "{{json .State}}"]
                && tail[2].len() == 64 =>
        {
            writeln!(io::stdout().lock(), "{{\"Status\":\"running\"}}")?;
        }
        "logs" if tail.len() == 2 && tail[0] == "--tail=200" && tail[1].len() == 64 => {
            writeln!(io::stdout().lock(), "retained fake native diagnostic")?;
        }
        "stop" if tail == ["--all", "--time=10"] => {
            if scenario == Scenario::HungCleanup {
                let mut command = Command::new("bash");
                ignored_command(&mut command);
                command.arg(std::env::current_exe()?).args([
                    "interruption-runtime",
                    "helper",
                    "forever",
                ]);
                return Err(command.exec().into());
            }
            if scenario == Scenario::CleanupFailure {
                return Ok(ExitCode::FAILURE);
            }
            state.running = false;
        }
        "rm" if tail == ["--all"] => state.containers = false,
        "rmi" if tail == ["--all"] => state.images = false,
        "system" if tail == ["migrate"] => state.migrated = true,
        _ => return Err(Error::Usage),
    }
    save(root, &state)?;
    Ok(ExitCode::SUCCESS)
}
pub fn run(args: &[OsString]) -> Result<ExitCode, Error> {
    let args: Vec<_> = args
        .iter()
        .map(|s| s.to_str().map(str::to_owned).ok_or(Error::Usage))
        .collect::<Result<_, _>>()?;
    let (name, args) = args.split_first().ok_or(Error::Usage)?;
    if name == "pipe-probe" && args.is_empty() {
        let mut helper = Command::new(std::env::current_exe()?)
            .args(["interruption-runtime", "helper", "short"])
            .spawn()?;
        writeln!(io::stdout().lock(), "ready")?;
        io::stdout().lock().flush()?;
        thread::sleep(Duration::from_secs(60));
        if !helper.wait()?.success() {
            return Err(Error::Readiness);
        }
        return Ok(ExitCode::SUCCESS);
    }
    if name == "helper" {
        match args {
            [mode] if mode == "forever" => sleep_forever(),
            [mode] if mode == "short" => {
                thread::sleep(Duration::from_secs(4));
                return Ok(ExitCode::SUCCESS);
            }
            [] => {}
            [mode, ready] if mode == "SIG_IGN" => fs::write(ready, "")?,
            _ => return Err(Error::Usage),
        }
        thread::sleep(Duration::from_secs(60));
        return Ok(ExitCode::SUCCESS);
    }
    let root = PathBuf::from(environment("WRIX_FAKE_NATIVE_ROOT")?);
    if name == "supervised-child" {
        let [action] = args else {
            return Err(Error::Usage);
        };
        let registry = PathBuf::from(environment("WRIX_TEST_POOL_FIXTURE_REGISTRY")?);
        fs::write(registry.join("root"), root.as_os_str().as_encoded_bytes())?;
        fs::write(root.join("ready"), "")?;
        return match action.as_str() {
            "success" => Ok(ExitCode::SUCCESS),
            "failure" => Ok(ExitCode::FAILURE),
            "killed" => {
                rustix::process::kill_process(
                    rustix::process::getpid(),
                    rustix::process::Signal::KILL,
                )
                .map_err(io::Error::from)?;
                sleep_forever()
            }
            "wait" => {
                thread::sleep(Duration::from_secs(60));
                Ok(ExitCode::SUCCESS)
            }
            _ => Err(Error::Usage),
        };
    }
    let scenario = match std::env::var_os("WRIX_FAKE_SCENARIO") {
        None => Scenario::Pass,
        Some(value) => Scenario::parse(value.to_str().ok_or(Error::Usage)?)?,
    };
    match name.as_str() {
        "native" | "native-ignored" if args.is_empty() => {
            native(&root, scenario, name == "native-ignored")
        }
        "git" if args == ["rev-parse", "HEAD"] => {
            writeln!(io::stdout().lock(), "{}", "a".repeat(40))?;
            Ok(ExitCode::SUCCESS)
        }
        "podman" => podman(&root, scenario, args),
        "readlink" if args == ["/proc/self/ns/mnt"] => {
            writeln!(
                io::stdout().lock(),
                "mnt:[{}]",
                if scenario == Scenario::ChangedNamespace && load(&root)?.migrated {
                    2
                } else {
                    1
                }
            )?;
            Ok(ExitCode::SUCCESS)
        }
        "cat" if args == ["/proc/self/mountinfo"] => {
            let state = load(&root)?;
            let views = if scenario == Scenario::PostMount && state.migrated {
                vec![View::C, View::D]
            } else {
                state.views
            };
            for view in views {
                let target = root
                    .join("home/.local/state/wrix/control/nix")
                    .join("b".repeat(32))
                    .join("x86_64-linux/executions")
                    .join(view.name())
                    .join("store");
                let escaped = target
                    .display()
                    .to_string()
                    .replace('\\', "\\134")
                    .replace(' ', "\\040");
                writeln!(
                    io::stdout().lock(),
                    "90 2 0:99 / {escaped} ro,nosuid,nodev - fuse.sshfs wrix-nix-{} ro",
                    "a".repeat(32)
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
        "fusermount3" if args.len() == 3 && args[..2] == ["-u", "--"] => {
            let mut state = load(&root)?;
            let key = Path::new(&args[2])
                .parent()
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
                .ok_or(Error::Usage)?;
            let position = state
                .views
                .iter()
                .position(|v| v.name() == key)
                .ok_or(Error::Usage)?;
            state.views.remove(position);
            save(&root, &state)?;
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(Error::Usage),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn invocation(root: &Path, operation: &[&str]) -> Vec<String> {
        let mut args = vec![
            "--remote=false".into(),
            "--root".into(),
            root.join("home/.local/share/containers/storage")
                .display()
                .to_string(),
            "--runroot".into(),
            root.join("runtime/containers").display().to_string(),
            "--tmpdir".into(),
            root.join("runtime/libpod/tmp").display().to_string(),
        ];
        args.extend(operation.iter().map(|s| (*s).to_owned()));
        args
    }

    #[test]
    fn cleanup_advances_independent_resources_only_when_requested() {
        let root = tempfile::tempdir().unwrap();
        save(root.path(), &State::initial(false)).unwrap();
        for (args, expected) in [
            (
                vec!["stop", "--all", "--time=10"],
                (false, true, true, false),
            ),
            (vec!["rm", "--all"], (false, false, true, false)),
            (vec!["rmi", "--all"], (false, false, false, false)),
            (vec!["system", "migrate"], (false, false, false, true)),
        ] {
            assert_eq!(
                podman(root.path(), Scenario::Pass, &invocation(root.path(), &args)).unwrap(),
                ExitCode::SUCCESS
            );
            let state = load(root.path()).unwrap();
            assert_eq!(
                (
                    state.running,
                    state.containers,
                    state.images,
                    state.migrated
                ),
                expected
            );
            assert_eq!(state.views.len(), 2);
        }
    }

    #[test]
    fn failed_stop_preserves_running_resources_for_final_inventory() {
        let root = tempfile::tempdir().unwrap();
        save(root.path(), &State::initial(false)).unwrap();
        let before = fs::read(root.path().join("fake-state.json")).unwrap();
        assert_eq!(
            podman(
                root.path(),
                Scenario::CleanupFailure,
                &invocation(root.path(), &["stop", "--all", "--time=10"])
            )
            .unwrap(),
            ExitCode::FAILURE
        );
        assert_eq!(
            fs::read(root.path().join("fake-state.json")).unwrap(),
            before
        );
    }

    #[test]
    fn foreign_storage_and_unsupported_runtime_arguments_fail_without_effects() {
        let root = tempfile::tempdir().unwrap();
        save(root.path(), &State::initial(false)).unwrap();
        let before = fs::read(root.path().join("fake-state.json")).unwrap();
        let mut args = invocation(root.path(), &["stop", "--all", "--time=10"]);
        args[2] = "/foreign/storage".into();
        assert!(matches!(
            podman(root.path(), Scenario::Pass, &args),
            Err(Error::Usage)
        ));
        assert!(matches!(
            podman(
                root.path(),
                Scenario::Pass,
                &invocation(root.path(), &["rm", "--force", "--all"])
            ),
            Err(Error::Usage)
        ));
        assert_eq!(
            fs::read(root.path().join("fake-state.json")).unwrap(),
            before
        );
    }

    #[test]
    fn checkpoint_binds_nonce_device_inode_and_private_mode() {
        let root = tempfile::tempdir().unwrap();
        let nonce = "a".repeat(64);
        record(root.path(), "workers-ready", ".ready", false, false, &nonce).unwrap();
        let path = root.path().join(".ready");
        let meta = root.path().metadata().unwrap();
        let record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            record,
            serde_json::json!({"version":1,"phase":"workers-ready","nonce":nonce,"device":meta.dev(),"inode":meta.ino()})
        );
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!root.path().join("checkpoint.pending").exists());
    }

    #[test]
    fn unsupported_scenario_is_rejected_instead_of_simulating_success() {
        assert!(matches!(Scenario::parse("unknown"), Err(Error::Usage)));
        assert_eq!(
            Scenario::parse("hung-supervisor").unwrap(),
            Scenario::HungSupervisor
        );
    }
    #[test]
    fn empty_readiness_has_no_resources_to_misreport() {
        let state = State::initial(true);
        assert!(!state.running && !state.containers && !state.images && state.views.is_empty());
    }
    #[test]
    fn running_fixture_preserves_both_independent_export_identities() {
        let state = State::initial(false);
        assert_eq!(
            serde_json::to_value(state).unwrap(),
            serde_json::json!({"running":true,"containers":true,"images":true,"views":["c","d"],"migrated":false})
        );
    }
}
