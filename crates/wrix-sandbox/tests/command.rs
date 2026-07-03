mod common;

use std::{fs, path::PathBuf};

use wrix_sandbox::command::{self, Command};

use common::{ChildSpec, ProfileFixture, TestResult};

#[test]
fn run_requires_valid_profile_config() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("command-config")
        .tempdir()?;
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;

    let missing = invoke(Command::Run, None, &[workspace_arg(&workspace)])?;
    assert!(!missing.success);
    assert!(missing.stderr.contains("--profile-config"));
    assert!(missing.stderr.contains("ProfileConfig"));

    let invalid_json = root.path().join("invalid.json");
    fs::write(&invalid_json, "{not-json\n")?;
    let invalid = invoke(
        Command::Run,
        Some(invalid_json),
        &[workspace_arg(&workspace)],
    )?;
    assert!(!invalid.success);
    assert!(invalid.stderr.contains("invalid ProfileConfig JSON"));

    let bad_schema = root.path().join("schema.json");
    fs::write(
        &bad_schema,
        r#"{"schema":2,"profile":{"name":"base"},"image":{"ref":"wrix:test","source":"/nix/store/fake","source_kind":"nix-descriptor"},"agent":{"kind":"direct"}}"#,
    )?;
    let schema = invoke(Command::Run, Some(bad_schema), &[workspace_arg(&workspace)])?;
    assert!(!schema.success);
    assert!(schema.stderr.contains("unsupported ProfileConfig schema"));

    Ok(())
}

#[test]
fn profile_config_agent_cannot_be_overridden_by_env() -> TestResult {
    let root = tempfile::Builder::new().prefix("agent-pin").tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    fs::create_dir_all(&workspace)?;
    common::write_profile_config(&profile_config, &ProfileFixture::default())?;

    let run = common::run_child(
        "command_child",
        root.path(),
        "agent-env",
        ChildSpec {
            command: Command::Run,
            profile_config: Some(profile_config),
            args: vec![workspace_arg(&workspace), String::from("true")],
            env: vec![(String::from("WRIX_AGENT"), "pi".into())],
        },
    )?;

    assert!(run.success, "{}", run.stderr);
    assert!(run.stderr.is_empty(), "{}", run.stderr);
    assert!(run.stdout.contains("PROFILE_AGENT=direct"));
    assert!(run.stdout.contains("ENV=WRIX_AGENT=direct"));
    assert!(!run.stdout.contains("ENV=WRIX_AGENT=pi"));

    Ok(())
}

#[test]
#[ignore = "child process receives per-test environment"]
fn command_child() -> TestResult {
    common::run_command_child()
}

struct Invocation {
    stderr: String,
    success: bool,
}

fn invoke(
    command: Command,
    profile_config: Option<PathBuf>,
    args: &[String],
) -> TestResult<Invocation> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = command::run(command, profile_config, args, &mut stdout, &mut stderr)?;
    assert!(stdout.is_empty(), "{}", String::from_utf8_lossy(&stdout));
    Ok(Invocation {
        stderr: String::from_utf8(stderr)?,
        success: code == std::process::ExitCode::SUCCESS,
    })
}

fn workspace_arg(path: &std::path::Path) -> String {
    path.display().to_string()
}
