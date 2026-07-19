mod common;

use std::fs;

use serde_json::{Value, json};
use wrix_sandbox::command::Command;

use common::{ChildSpec, ProfileFixture, TestResult};

#[test]
fn documented_spawn_config_fields_render_into_launch_plan() -> TestResult {
    let fixture = SpawnFixture::new("spawn-documented")?;
    let mount_host = fixture.root.path().join("documented-mount");
    fs::create_dir_all(&mount_host)?;
    let config = fixture.write(
        "documented",
        &json!({
            "workspace": path_text(&fixture.workspace),
            "image_ref": "wrix-override:test",
            "image_source": "/nix/store/fake-override",
            "image_source_kind": common::expected_source_kind(),
            "env": [["FOO", "bar"], ["EMPTY", ""]],
            "agent_args": ["--print", "hello"],
            "mounts": [{"host_path": path_text(&mount_host), "container_path": "/mnt/schema", "read_only": true}]
        }),
    )?;

    let run = fixture.run("documented", &config)?;

    assert!(run.success, "{}", run.stderr);
    assert!(run.stderr.is_empty(), "{}", run.stderr);
    for expected in [
        "STDIO=1",
        "PROFILE_AGENT=direct",
        "ENV=WRIX_AGENT=direct",
        "IMAGE_OVERRIDE_REF=wrix-override:test",
        "IMAGE_OVERRIDE_SOURCE=/nix/store/fake-override",
        "ENV=FOO=bar",
        "ENV=EMPTY=",
        "CMD=--print",
        "CMD=hello",
        "/mnt/schema",
    ] {
        assert!(
            run.stdout.contains(expected),
            "missing {expected}: {}",
            run.stdout
        );
    }
    assert!(run.stdout.contains(&format!(
        "IMAGE_OVERRIDE_SOURCE_KIND={}",
        common::expected_source_kind()
    )));
    Ok(())
}

#[test]
fn consumer_spawn_config_fields_are_mounted_for_entrypoint() -> TestResult {
    let fixture = SpawnFixture::new("spawn-consumer-fields")?;
    let config = fixture.write(
        "consumer-fields",
        &json!({
            "workspace": path_text(&fixture.workspace),
            "env": [],
            "agent_args": [],
            "mounts": [],
            "initial_prompt": "consumer field",
            "repin": true
        }),
    )?;

    let run = fixture.run("consumer-fields", &config)?;

    assert!(run.success, "{}", run.stderr);
    assert!(run.stdout.contains("ENV=WRIX_SPAWN_CONFIG="));
    assert!(run.stdout.contains(&config.display().to_string()));
    assert!(run.stdout.contains("spawn-config.json:ro"));
    Ok(())
}

#[test]
fn provider_credentials_in_spawn_config_are_redacted() -> TestResult {
    let fixture = SpawnFixture::new("spawn-provider-redaction")?;
    let config = fixture.write(
        "provider-redaction",
        &json!({
            "workspace": path_text(&fixture.workspace),
            "env": [["OPENAI_API_KEY", "spawn-secret"]],
            "agent_args": [],
            "mounts": []
        }),
    )?;

    let run = fixture.run("provider-redaction", &config)?;

    assert!(run.success, "{}", run.stderr);
    assert!(run.stdout.contains("ENV=OPENAI_API_KEY=[REDACTED]"));
    assert!(!run.stdout.contains("spawn-secret"));
    Ok(())
}

#[test]
fn image_source_override_requires_source_kind() -> TestResult {
    let fixture = SpawnFixture::new("spawn-missing-kind")?;
    let config = fixture.write(
        "without-kind",
        &json!({
            "workspace": path_text(&fixture.workspace),
            "image_ref": "wrix-override:test",
            "image_source": "/nix/store/fake-override",
            "env": [],
            "agent_args": [],
            "mounts": []
        }),
    )?;

    let run = fixture.run("without-kind", &config)?;

    assert!(!run.success);
    assert!(
        run.stderr
            .contains("image_source requires image_source_kind")
    );
    Ok(())
}

#[test]
fn image_source_kind_must_match_platform() -> TestResult {
    let fixture = SpawnFixture::new("spawn-incompatible-kind")?;
    let config = fixture.write(
        "incompatible-kind",
        &json!({
            "workspace": path_text(&fixture.workspace),
            "image_ref": "wrix-override:test",
            "image_source": "/nix/store/fake-override",
            "image_source_kind": alternate_source_kind(),
            "env": [],
            "agent_args": [],
            "mounts": []
        }),
    )?;

    let run = fixture.run("incompatible-kind", &config)?;

    assert!(!run.success);
    assert!(run.stderr.contains(&format!(
        "image_source_kind must be {}",
        common::expected_source_kind()
    )));
    Ok(())
}

#[test]
fn spawn_config_cannot_override_agent() -> TestResult {
    let fixture = SpawnFixture::new("spawn-agent-override")?;
    let config = fixture.write(
        "agent-override",
        &json!({
            "workspace": path_text(&fixture.workspace),
            "env": [],
            "agent_args": [],
            "mounts": [],
            "agent": { "kind": "pi" }
        }),
    )?;

    let run = fixture.run("agent-override", &config)?;

    assert!(!run.success);
    assert!(
        run.stderr
            .contains("SpawnConfig cannot change the ProfileConfig agent")
    );
    Ok(())
}

#[test]
fn linux_spawn_mounts_render_podman_volume_args() -> TestResult {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }

    let fixture = SpawnFixture::new("spawn-mounts")?;
    let rw = fixture.root.path().join("rw-src");
    let ro = fixture.root.path().join("ro-src");
    let two = fixture.write(
        "two",
        &json!({
            "workspace": path_text(&fixture.workspace),
            "env": [],
            "agent_args": [],
            "mounts": [
                {"host_path": path_text(&rw), "container_path": "/mnt/rw", "read_only": false},
                {"host_path": path_text(&ro), "container_path": "/mnt/ro", "read_only": true}
            ]
        }),
    )?;
    let two_run = fixture.run("two-mounts", &two)?;
    assert!(two_run.success, "{}", two_run.stderr);
    let mounts = spawn_mount_lines(&two_run.stdout);
    assert_eq!(mounts.len(), 2, "{}", two_run.stdout);
    assert!(mounts.iter().any(|line| line.ends_with(":/mnt/rw")));
    assert!(mounts.iter().any(|line| line.ends_with(":/mnt/ro:ro")));

    for (label, value) in [
        (
            "missing-mounts",
            json!({"workspace": path_text(&fixture.workspace), "env": [], "agent_args": []}),
        ),
        (
            "empty-mounts",
            json!({"workspace": path_text(&fixture.workspace), "env": [], "agent_args": [], "mounts": []}),
        ),
    ] {
        let config = fixture.write(label, &value)?;
        let run = fixture.run(label, &config)?;
        assert!(run.success, "{}", run.stderr);
        assert!(spawn_mount_lines(&run.stdout).is_empty(), "{}", run.stdout);
    }
    Ok(())
}

#[test]
#[ignore = "child process receives per-test environment"]
fn spawn_config_child() -> TestResult {
    common::run_command_child()
}

struct SpawnFixture {
    root: tempfile::TempDir,
    workspace: std::path::PathBuf,
    profile_config: std::path::PathBuf,
}

impl SpawnFixture {
    fn new(prefix: &str) -> TestResult<Self> {
        let root = tempfile::Builder::new().prefix(prefix).tempdir()?;
        let workspace = root.path().join("workspace");
        let profile_config = root.path().join("profile.json");
        fs::create_dir_all(&workspace)?;
        let key_dir = root.path().join("home/.ssh/deploy_keys");
        fs::create_dir_all(&key_dir)?;
        fs::write(key_dir.join("repo-key"), "private key\n")?;
        fs::write(key_dir.join("repo-key-signing"), "signing key\n")?;
        common::write_profile_config(
            &profile_config,
            &ProfileFixture {
                deploy_key: Some(String::from("repo-key")),
                ..ProfileFixture::default()
            },
        )?;
        Ok(Self {
            root,
            workspace,
            profile_config,
        })
    }

    fn write(&self, label: &str, value: &Value) -> TestResult<std::path::PathBuf> {
        let path = self.root.path().join(format!("{label}.json"));
        fs::write(&path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
        Ok(path)
    }

    fn run(&self, label: &str, spawn_config: &std::path::Path) -> TestResult<common::ChildRun> {
        common::run_child(
            "spawn_config_child",
            self.root.path(),
            label,
            ChildSpec {
                command: Command::Spawn,
                profile_config: Some(self.profile_config.clone()),
                args: vec![
                    String::from("--spawn-config"),
                    spawn_config.display().to_string(),
                    String::from("--stdio"),
                ],
                env: Vec::new(),
            },
        )
    }
}

fn spawn_mount_lines(output: &str) -> Vec<&str> {
    output
        .lines()
        .filter(|line| line.starts_with("MOUNT=-v ") && !line.contains("spawn-config.json"))
        .collect()
}

fn path_text(path: &std::path::Path) -> String {
    path.display().to_string()
}

const fn alternate_source_kind() -> &'static str {
    if cfg!(target_os = "macos") {
        "nix-descriptor"
    } else {
        "docker-archive"
    }
}
