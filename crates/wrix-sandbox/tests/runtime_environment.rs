mod common;
#[path = "common/runtime_environment.rs"]
mod runtime_environment;

use common::{ChildSpec, ProfileFixture, TestResult};
use serde_json::json;
use std::{
    env, fs,
    os::unix::fs::DirBuilderExt,
    process::{Command as ProcessCommand, Stdio},
};
use wrix_sandbox::command::Command;

#[test]
#[ignore = "requires owning protocol image and an actual native container runtime"]
fn actual_native_guest_consumes_the_launchers_private_environment_without_host_argv_values()
-> TestResult {
    let image = env::var("WRIX_TEST_RUNTIME_ENV_IMAGE")?;
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let profile = root.path().join("profile.json");
    common::write_profile_config(&profile, &ProfileFixture::default())?;
    let secret = "synthetic-protocol-api-key";
    let message = "first=one\r\nsecond=two\n";
    let config = root.path().join("spawn.json");
    fs::write(
        &config,
        serde_json::to_vec(&json!({
            "workspace": workspace, "env": [["OPENAI_API_KEY", secret], ["EMPTY", ""], ["MESSAGE", message], ["CONTAINER_HOST", "guest-only-endpoint"]],
            "agent_args": [], "mounts": []
        }))?,
    )?;
    let run = common::run_child(
        "runtime_environment_child",
        root.path(),
        "producer",
        ChildSpec {
            command: Command::Spawn,
            profile_config: Some(profile),
            args: vec![
                "--spawn-config".into(),
                config.display().to_string(),
                "--stdio".into(),
            ],
            env: vec![
                ("CONTAINER_HOST".into(), "host-selected-endpoint".into()),
                (
                    "WRIX_TEST_EXPECT_CONTAINER_HOST".into(),
                    "host-selected-endpoint".into(),
                ),
            ],
            capture_runtime: true,
        },
    )?;
    assert!(run.success, "{}", run.stderr);
    assert_eq!(
        run.guest_environment
            .get("OPENAI_API_KEY")
            .map(String::as_str),
        Some(secret)
    );
    assert!(!run.stdout.contains(secret));
    assert!(!run.stdout.contains("guest-only-endpoint"));
    let source = root.path().join("native-environment");
    fs::DirBuilder::new().mode(0o700).create(&source)?;
    fs::copy(
        root.path().join("argv.environment"),
        source.join("environment"),
    )?;
    let stdout = fs::File::create(root.path().join("guest.out"))?;
    let stderr = fs::File::create(root.path().join("guest.err"))?;
    let mut command = ProcessCommand::new(if cfg!(target_os = "macos") {
        "container"
    } else {
        "podman"
    });
    command
        .args([
            "run",
            "--rm",
            "--cap-drop",
            "ALL",
            "--network",
            "none",
            "--cpus",
            "1",
            "--memory",
            "256m",
            "-v",
        ])
        .arg(format!(
            "{}:/run/wrix/runtime-environment:ro",
            source.display()
        ))
        .arg(image)
        .arg("agent")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    assert!(
        command
            .get_args()
            .all(|arg| !arg.to_string_lossy().contains(secret))
    );
    assert_eq!(command.get_envs().count(), 0);
    let mut child = command.spawn()?;
    let observed = ProcessCommand::new("ps")
        .args(["-ww", "-p", &child.id().to_string(), "-o", "command="])
        .output()?;
    assert!(observed.status.success());
    assert!(!String::from_utf8(observed.stdout)?.contains(secret));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err("native environment consumer exceeded 90 seconds".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    assert!(
        status.success(),
        "{}",
        fs::read_to_string(root.path().join("guest.err"))?
    );
    let expected = format!("{secret}\0\0{message}\0guest-only-endpoint\01\0");
    assert_eq!(
        fs::read(root.path().join("guest.out"))?,
        expected.as_bytes()
    );
    Ok(())
}

#[test]
#[ignore = "subprocess dispatcher, not an independently scheduled scenario"]
fn runtime_environment_child() -> TestResult {
    common::run_command_child()
}
