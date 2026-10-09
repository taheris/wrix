#[path = "common/lifecycle/mod.rs"]
mod lifecycle;

use std::{
    env, fs, io,
    io::{Read, Seek, SeekFrom},
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use lifecycle::{Control, Fixture, Process, Request, TestResult};
use serde_json::{Value, json};
use wrix_sandbox::command::{self, Command as Mode};

#[test]
fn launch_records_precede_work_and_do_not_collide() -> TestResult {
    for mode in [Mode::Run, Mode::Spawn] {
        let first = Fixture::new()?;
        let second = Fixture::new()?;
        configure_identity(&first)?;
        configure_identity(&second)?;
        enable_service(&first)?;
        enable_service(&second)?;
        let workspace = first.workspace();
        next_second()?;
        let mut a = start(&first, mode, &workspace, 37)?;
        let mut b = start(&second, mode, &workspace, 37)?;
        let mut control_a = first.accept(&mut a)?;
        let mut control_b = second.accept(&mut b)?;
        assert_eq!(control_a.receive()?, "ready service");
        assert_eq!(control_b.receive()?, "ready service");
        let paths = records(&workspace)?;
        assert_eq!(paths.len(), 2);
        let values = paths
            .iter()
            .map(|path| read_record(path))
            .collect::<TestResult<Vec<_>>>()?;
        assert_ne!(values[0]["execution_id"], values[1]["execution_id"]);
        assert_eq!(
            values[0]["timestamp_start"].as_str().unwrap()[..19],
            values[1]["timestamp_start"].as_str().unwrap()[..19]
        );
        for (path, value) in paths.iter().zip(&values) {
            assert_incomplete(value);
            assert_eq!(value["mode"], mode_name(mode));
            assert_eq!(value["agent_kind"], "direct");
            assert_eq!(value["focus_target"], "host:2.1");
            assert_eq!(
                value["bead_id"],
                if mode == Mode::Spawn {
                    json!("wx-test.16")
                } else {
                    Value::Null
                }
            );
            assert_eq!(
                path.file_stem().unwrap().to_str().unwrap(),
                value["execution_id"].as_str().unwrap()
            );
            assert!(
                path.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(&value["timestamp_start"].as_str().unwrap()[..10])
            );
            assert!(!serde_json::to_string(value)?.contains("fixture-provider-secret"));
        }
        for (child, control) in [(&mut a, &mut control_a), (&mut b, &mut control_b)] {
            release(control, "finished 37")?;
            assert_eq!(child.finish()?.code(), Some(1));
        }
        assert_eq!(records(&workspace)?, paths);
        for path in &paths {
            let value = read_record(path)?;
            assert_completed(&value);
            assert!(value["exit_code"].is_null());
            assert!(value["signal"].is_null());
        }
    }
    Ok(())
}

#[test]
fn observed_termination_updates_the_original_record() -> TestResult {
    for mode in [Mode::Run, Mode::Spawn] {
        for outcome in [Outcome::Exit(0), Outcome::Exit(37), Outcome::Signal] {
            let fixture = Fixture::new()?;
            let workspace = fixture.workspace();
            let (mut child, mut control) = running(&fixture, mode, outcome)?;
            let paths = records(&workspace)?;
            assert_eq!(paths.len(), 1);
            let path = &paths[0];
            let initial = read_record(path)?;
            assert_incomplete(&initial);
            let mut original = fs::File::open(path)?;
            let inode = original.metadata()?.ino();
            let reader_path = path.clone();
            let reader = thread::spawn(move || -> Result<(), String> {
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    let value = read_record(&reader_path).map_err(|error| error.to_string())?;
                    match value["state"].as_str() {
                        Some("incomplete") => assert_incomplete(&value),
                        Some("completed") => {
                            assert_completed(&value);
                            return Ok(());
                        }
                        other => return Err(format!("invalid state: {other:?}")),
                    }
                    thread::yield_now();
                }
                Err("atomic completion reader timed out".into())
            });
            release(&mut control, outcome.ack())?;
            assert_eq!(child.finish()?.code(), Some(outcome.caller_code()));
            reader.join().map_err(|_| "reader panicked")??;
            assert_eq!(records(&workspace)?, paths);
            assert_ne!(fs::metadata(path)?.ino(), inode);
            original.seek(SeekFrom::Start(0))?;
            let mut original_bytes = Vec::new();
            original.read_to_end(&mut original_bytes)?;
            assert_eq!(serde_json::from_slice::<Value>(&original_bytes)?, initial);
            let completed = read_record(path)?;
            assert_completed(&completed);
            assert_eq!(completed["execution_id"], initial["execution_id"]);
            assert_eq!(completed["timestamp_start"], initial["timestamp_start"]);
            assert_status(&completed, outcome);
            assert!(completed["agent_session_id"].is_null());
        }
        for exit in [0, 37] {
            let fixture = Fixture::new()?;
            fs::write(fixture.workspace().join(".wrix"), "not a directory")?;
            let mut child = start(&fixture, mode, &fixture.workspace(), exit)?;
            assert_eq!(child.finish()?.code(), Some(1));
            assert!(
                fixture
                    .diagnostics()?
                    .contains("execution metadata I/O failed")
            );
            assert!(!fixture.root().join("argv").exists());
        }
        let fixture = Fixture::new()?;
        enable_service(&fixture)?;
        fs::write(fixture.workspace().join(".wrix"), "not a directory")?;
        let mut child = start(&fixture, mode, &fixture.workspace(), 37)?;
        assert_eq!(child.finish()?.code(), Some(1));
        assert!(!fixture.root().join("argv").exists());
    }
    Ok(())
}

#[test]
fn completion_update_errors_preserve_launch_outcome() -> TestResult {
    for mode in [Mode::Run, Mode::Spawn] {
        for point in [
            FailurePoint::TemporaryCreation,
            FailurePoint::AtomicReplacement,
        ] {
            for outcome in [Outcome::Exit(0), Outcome::Exit(37), Outcome::Signal] {
                let fixture = Fixture::new()?;
                let (mut child, mut control) = running(&fixture, mode, outcome)?;
                let obstruction = obstruct_completion(&fixture.workspace(), point)?;
                release(&mut control, outcome.ack())?;
                assert_eq!(child.finish()?.code(), Some(outcome.caller_code()));
                obstruction.restore()?;
                assert_incomplete(&read_record(&records(&fixture.workspace())?[0])?);
                assert!(
                    fixture
                        .diagnostics()?
                        .contains("completion metadata update failed")
                );
            }
            let fixture = Fixture::new()?;
            enable_service(&fixture)?;
            let mut child = start(&fixture, mode, &fixture.workspace(), 37)?;
            let mut control = fixture.accept(&mut child)?;
            assert_eq!(control.receive()?, "ready service");
            let obstruction = obstruct_completion(&fixture.workspace(), point)?;
            release(&mut control, "finished 37")?;
            assert_eq!(child.finish()?.code(), Some(1));
            obstruction.restore()?;
            assert_incomplete(&read_record(&records(&fixture.workspace())?[0])?);
            let diagnostic = fixture.diagnostics()?;
            assert!(diagnostic.contains("completion metadata update failed"));
            assert!(diagnostic.contains("service command failed"));
        }
    }
    Ok(())
}

#[test]
fn metadata_status_tracks_the_container_runtime_command() -> TestResult {
    for mode in [Mode::Run, Mode::Spawn] {
        for outcome in [
            Outcome::Exit(0),
            Outcome::Exit(37),
            Outcome::Exit(137),
            Outcome::Signal,
        ] {
            let fixture = Fixture::new()?;
            let (mut child, mut control) = running(&fixture, mode, outcome)?;
            release(&mut control, outcome.ack())?;
            assert_eq!(child.finish()?.code(), Some(outcome.caller_code()));
            let value = read_record(&records(&fixture.workspace())?[0])?;
            assert_completed(&value);
            assert_status(&value, outcome);
        }
        let fixture = Fixture::new()?;
        enable_service(&fixture)?;
        let mut child = start(&fixture, mode, &fixture.workspace(), 37)?;
        let mut control = fixture.accept(&mut child)?;
        assert_eq!(control.receive()?, "ready service");
        release(&mut control, "finished 37")?;
        assert_eq!(child.finish()?.code(), Some(1));
        let value = read_record(&records(&fixture.workspace())?[0])?;
        assert_completed(&value);
        assert!(value["exit_code"].is_null());
        assert!(value["signal"].is_null());
    }
    Ok(())
}

#[test]
fn session_roots_use_container_absolute_paths() -> TestResult {
    for mode in [Mode::Run, Mode::Spawn] {
        for (agent, root) in [
            ("claude", Some("/workspace/.claude")),
            ("pi", Some("/workspace/.pi/agent/sessions")),
            ("direct", None),
        ] {
            let fixture = Fixture::new()?;
            edit_json(&fixture.profile, |value| {
                value["agent"]["kind"] = json!(agent);
            })?;
            let auth = fixture.root().join("auth.json");
            fs::write(&auth, "{}")?;
            fs::create_dir_all(fixture.workspace().join(".claude"))?;
            fs::write(
                fixture.workspace().join(".claude/history.jsonl"),
                "{\"sessionId\":\"unrelated-conversation\"}\n",
            )?;
            let mut command = configured_command(&fixture, mode, &fixture.workspace(), 0)?;
            command
                .env("WRIX_PI_AUTH_FILE", &auth)
                .env("PI_SESSION_ID", "not-attributable")
                .env("WRIX_SESSION_ID", "legacy-focus");
            let mut child = Process::spawn(&mut command)?;
            let mut control = fixture.accept(&mut child)?;
            assert!(control.receive()?.starts_with("ready "));
            let path = records(&fixture.workspace())?.remove(0);
            let value = read_record(&path)?;
            assert!(
                fixture
                    .argv()?
                    .contains(&format!("{}:/workspace", fixture.workspace().display()))
            );
            assert_eq!(value["agent_session_dir"], json!(root));
            assert!(value["agent_session_id"].is_null());
            assert!(value.get("wrix_session_id").is_none());
            assert!(value.get("claude_session_id").is_none());
            if let Some(root) = root {
                let relative = Path::new(root).strip_prefix("/workspace")?;
                let host = fixture.workspace().join(relative);
                fs::create_dir_all(&host)?;
                fs::write(host.join("session-marker"), "container-visible root")?;
                assert_eq!(
                    fs::read_to_string(fixture.workspace().join(relative).join("session-marker"))?,
                    "container-visible root"
                );
                assert!(
                    !serde_json::to_string(&value)?.contains(fixture.workspace().to_str().unwrap())
                );
            }
            release(&mut control, "finished 0")?;
            assert_eq!(child.finish()?.code(), Some(0));
            assert_eq!(read_record(&path)?["agent_session_dir"], json!(root));
        }
    }
    Ok(())
}

#[test]
fn killed_launcher_leaves_incomplete_metadata() -> TestResult {
    for mode in [Mode::Run, Mode::Spawn] {
        let fixture = Fixture::new()?;
        let (child, mut control) = running(&fixture, mode, Outcome::Exit(0))?;
        let paths = records(&fixture.workspace())?;
        drop(child);
        assert!(control.receive().is_err());
        assert_incomplete(&read_record(&paths[0])?);
    }
    Ok(())
}

#[test]
fn invalid_launch_inputs_do_not_create_records() -> TestResult {
    for mode in [Mode::Run, Mode::Spawn] {
        let fixture = Fixture::new()?;
        enable_service(&fixture)?;
        let mut command = configured_command(&fixture, mode, &fixture.workspace(), 0)?;
        command.env("WRIX_NETWORK", "invalid");
        assert_eq!(Process::spawn(&mut command)?.finish()?.code(), Some(1));
        assert!(!fixture.workspace().join(".wrix").exists());
        assert!(!fixture.root().join("argv").exists());
    }
    Ok(())
}

#[test]
fn metadata_launch_preserves_foreground_fixture_contract() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.assert_foreground(child_command()?, false, 37)?;
    let argv = fixture.launched_argv(child_command()?, &[])?;
    assert_eq!(argv[0], "run");
    Ok(())
}

#[test]
fn service_runtime_fixture_child() -> TestResult {
    if env::args().nth(1).as_deref() != Some("service") {
        return Ok(());
    }
    let error =
        Command::new(env::var_os("WRIX_TEST_SERVICE_BIN").ok_or("service fixture missing")?)
            .args(env::args().skip(1))
            .exec();
    Err(error.into())
}

#[test]
#[ignore = "isolated launcher process"]
fn metadata_child() -> TestResult {
    let profile = PathBuf::from(env::var_os("WRIX_TEST_PROFILE_CONFIG").ok_or("profile missing")?);
    let mode = match env::var("WRIX_TEST_MODE") {
        Ok(value) if value == "run" => Mode::Run,
        Ok(_) | Err(env::VarError::NotPresent) => Mode::Spawn,
        Err(error) => return Err(error.into()),
    };
    let args = match mode {
        Mode::Run => vec![env::var("WRIX_TEST_WORKSPACE")?],
        Mode::Spawn => vec![
            "--spawn-config".to_owned(),
            env::var("WRIX_TEST_SPAWN_CONFIG")?,
        ],
    };
    let code = command::run(
        mode,
        Some(profile),
        &args,
        &mut io::stdout(),
        &mut io::stderr(),
    )?;
    for value in 0..=u8::MAX {
        if code == ExitCode::from(value) {
            std::process::exit(i32::from(value));
        }
    }
    Err("unsupported exit code".into())
}

#[derive(Clone, Copy)]
enum Outcome {
    Exit(u8),
    Signal,
}

impl Outcome {
    const fn caller_code(self) -> i32 {
        match self {
            Self::Exit(code) => code as i32,
            Self::Signal => 1,
        }
    }
    const fn ack(self) -> &'static str {
        match self {
            Self::Exit(0) => "finished 0",
            Self::Exit(37) => "finished 37",
            Self::Exit(_) => "finished 137",
            Self::Signal => "finished signal",
        }
    }
}

fn child_command() -> TestResult<Command> {
    let mut command = Command::new(env::current_exe()?);
    command.args(["metadata_child", "--exact", "--ignored"]);
    Ok(command)
}

fn configured_command(
    fixture: &Fixture,
    mode: Mode,
    workspace: &Path,
    exit: u8,
) -> TestResult<Command> {
    edit_json(&fixture.spawn, |value| {
        value["workspace"] = json!(workspace);
    })?;
    let mut command = child_command()?;
    fixture.configure(&mut command, exit)?;
    command
        .env("WRIX_TEST_MODE", mode_name(mode))
        .env("WRIX_TEST_WORKSPACE", workspace)
        .env("WRIX_FOCUS_TARGET", "host:2.1")
        .env("WRIX_TEST_SERVICE_BIN", fixture.root().join("bin/service"));
    Ok(command)
}

fn start(fixture: &Fixture, mode: Mode, workspace: &Path, exit: u8) -> TestResult<Process> {
    Ok(Process::spawn(&mut configured_command(
        fixture, mode, workspace, exit,
    )?)?)
}

fn running(fixture: &Fixture, mode: Mode, outcome: Outcome) -> TestResult<(Process, Control)> {
    let exit = match outcome {
        Outcome::Exit(code) => code,
        Outcome::Signal => 0,
    };
    let mut command = configured_command(fixture, mode, &fixture.workspace(), exit)?;
    if matches!(outcome, Outcome::Signal) {
        command.env("WRIX_TEST_SIGNAL", "TERM");
    }
    let mut child = Process::spawn(&mut command)?;
    let mut control = fixture.accept(&mut child)?;
    assert!(control.receive()?.starts_with("ready "));
    Ok((child, control))
}

fn release(control: &mut Control, expected: &str) -> TestResult {
    control.send(Request::Release)?;
    assert_eq!(control.receive()?, expected);
    Ok(())
}

fn edit_json(path: &Path, change: impl FnOnce(&mut Value)) -> TestResult {
    let mut value = serde_json::from_slice(&fs::read(path)?)?;
    change(&mut value);
    fs::write(path, serde_json::to_vec(&value)?)?;
    Ok(())
}

fn configure_identity(fixture: &Fixture) -> TestResult {
    edit_json(&fixture.spawn, |value| {
        value["bead_id"] = json!("wx-test.16");
        value["env"] = json!([
            ["WRIX_FOCUS_TARGET", "host:2.1"],
            ["OPENAI_API_KEY", "fixture-provider-secret"]
        ]);
    })
}

fn enable_service(fixture: &Fixture) -> TestResult {
    edit_json(&fixture.profile, |value| {
        value["services"]["nix_cache"]["enable"] = json!(true);
    })
}

fn records(workspace: &Path) -> TestResult<Vec<PathBuf>> {
    let mut paths = fs::read_dir(workspace.join(".wrix/log"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    assert!(
        paths
            .iter()
            .all(|path| path.extension().is_some_and(|ext| ext == "json"))
    );
    Ok(paths)
}

fn read_record(path: &Path) -> TestResult<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn assert_incomplete(value: &Value) {
    assert_eq!(value["state"], "incomplete");
    for field in [
        "timestamp_end",
        "duration_seconds",
        "exit_code",
        "signal",
        "agent_session_id",
    ] {
        assert_eq!(value.get(field), Some(&Value::Null), "{field}: {value}");
    }
}

fn assert_completed(value: &Value) {
    assert_eq!(value["state"], "completed");
    assert!(
        value["duration_seconds"]
            .as_f64()
            .is_some_and(|duration| duration >= 0.0)
    );
    for field in ["timestamp_start", "timestamp_end"] {
        assert!(
            value[field]
                .as_str()
                .is_some_and(|timestamp| timestamp.ends_with('Z'))
        );
    }
}

fn assert_status(value: &Value, outcome: Outcome) {
    match outcome {
        Outcome::Exit(code) => {
            assert_eq!(value["exit_code"], code);
            assert!(value["signal"].is_null());
        }
        Outcome::Signal => {
            assert!(value["exit_code"].is_null());
            assert_eq!(value["signal"], "SIGTERM");
        }
    }
}

#[derive(Clone, Copy)]
enum FailurePoint {
    TemporaryCreation,
    AtomicReplacement,
}

struct Obstruction {
    point: FailurePoint,
    original: PathBuf,
    saved: PathBuf,
}

impl Obstruction {
    fn restore(self) -> TestResult {
        match self.point {
            FailurePoint::TemporaryCreation => fs::remove_file(&self.original)?,
            FailurePoint::AtomicReplacement => fs::remove_dir(&self.original)?,
        }
        fs::rename(self.saved, self.original)?;
        Ok(())
    }
}

fn obstruct_completion(workspace: &Path, point: FailurePoint) -> TestResult<Obstruction> {
    let original = match point {
        FailurePoint::TemporaryCreation => workspace.join(".wrix/log"),
        FailurePoint::AtomicReplacement => records(workspace)?.remove(0),
    };
    let saved = original.with_extension("saved");
    fs::rename(&original, &saved)?;
    match point {
        FailurePoint::TemporaryCreation => fs::write(&original, "completion-only obstruction")?,
        FailurePoint::AtomicReplacement => fs::create_dir(&original)?,
    }
    Ok(Obstruction {
        point,
        original,
        saved,
    })
}

const fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Run => "run",
        Mode::Spawn => "spawn",
    }
}

fn next_second() -> TestResult {
    let current = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    while SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() == current {
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}
