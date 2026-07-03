mod common;

use std::fs;

use serde_json::{Value, json};
use wrix_sandbox::command::Command;

use common::{ChildSpec, ProfileFixture, TestResult};

#[test]
fn spawn_config_schema_and_agent_pin() -> TestResult {
    let root = tempfile::Builder::new().prefix("spawn-schema").tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    let mount_host = root.path().join("documented-mount");
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&mount_host)?;
    write_profile_config_with_keys(root.path(), &profile_config)?;

    let documented = root.path().join("documented.json");
    write_json(
        &documented,
        &json!({
            "workspace": path_text(&workspace),
            "image_ref": "wrix-override:test",
            "image_source": "/nix/store/fake-override",
            "image_source_kind": common::expected_source_kind(),
            "env": [["FOO", "bar"], ["EMPTY", ""]],
            "agent_args": ["--print", "hello"],
            "mounts": [{"host_path": path_text(&mount_host), "container_path": "/mnt/schema", "read_only": true}],
            "initial_prompt": "consumer field",
            "repin": true
        }),
    )?;
    let run = run_spawn(root.path(), "documented", &profile_config, &documented)?;
    assert!(run.success, "{}", run.stderr);
    assert!(run.stderr.is_empty(), "{}", run.stderr);
    assert!(run.stdout.contains("STDIO=1"));
    assert!(run.stdout.contains("PROFILE_AGENT=direct"));
    assert!(run.stdout.contains("ENV=WRIX_AGENT=direct"));
    assert!(run.stdout.contains("IMAGE_OVERRIDE_REF=wrix-override:test"));
    assert!(
        run.stdout
            .contains("IMAGE_OVERRIDE_SOURCE=/nix/store/fake-override")
    );
    assert!(run.stdout.contains(&format!(
        "IMAGE_OVERRIDE_SOURCE_KIND={}",
        common::expected_source_kind()
    )));
    assert!(run.stdout.contains("ENV=FOO=bar"));
    assert!(run.stdout.contains("ENV=EMPTY="));
    assert!(run.stdout.contains("CMD=--print"));
    assert!(run.stdout.contains("CMD=hello"));
    assert!(run.stdout.contains("/mnt/schema"));

    let without_kind = root.path().join("without-kind.json");
    write_json(
        &without_kind,
        &json!({
            "workspace": path_text(&workspace),
            "image_ref": "wrix-override:test",
            "image_source": "/nix/store/fake-override",
            "env": [],
            "agent_args": [],
            "mounts": []
        }),
    )?;
    let missing_kind = run_spawn(root.path(), "without-kind", &profile_config, &without_kind)?;
    assert!(!missing_kind.success);
    assert!(
        missing_kind
            .stderr
            .contains("image_source requires image_source_kind")
    );

    let incompatible = root.path().join("incompatible-kind.json");
    write_json(
        &incompatible,
        &json!({
            "workspace": path_text(&workspace),
            "image_ref": "wrix-override:test",
            "image_source": "/nix/store/fake-override",
            "image_source_kind": alternate_source_kind(),
            "env": [],
            "agent_args": [],
            "mounts": []
        }),
    )?;
    let bad_kind = run_spawn(
        root.path(),
        "incompatible-kind",
        &profile_config,
        &incompatible,
    )?;
    assert!(!bad_kind.success);
    assert!(bad_kind.stderr.contains(&format!(
        "image_source_kind must be {}",
        common::expected_source_kind()
    )));

    let agent_override = root.path().join("agent-override.json");
    write_json(
        &agent_override,
        &json!({
            "workspace": path_text(&workspace),
            "env": [],
            "agent_args": [],
            "mounts": [],
            "agent": { "kind": "pi" }
        }),
    )?;
    let override_run = run_spawn(
        root.path(),
        "agent-override",
        &profile_config,
        &agent_override,
    )?;
    assert!(!override_run.success);
    assert!(
        override_run
            .stderr
            .contains("SpawnConfig cannot change the ProfileConfig agent")
    );

    Ok(())
}

#[test]
fn linux_spawn_mounts_render_podman_volume_args() -> TestResult {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }

    let root = tempfile::Builder::new().prefix("spawn-mounts").tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    fs::create_dir_all(&workspace)?;
    write_profile_config_with_keys(root.path(), &profile_config)?;

    let rw = root.path().join("rw-src");
    let ro = root.path().join("ro-src");
    let two = root.path().join("two.json");
    write_json(
        &two,
        &json!({
            "workspace": path_text(&workspace),
            "env": [],
            "agent_args": [],
            "mounts": [
                {"host_path": path_text(&rw), "container_path": "/mnt/rw", "read_only": false},
                {"host_path": path_text(&ro), "container_path": "/mnt/ro", "read_only": true}
            ]
        }),
    )?;
    let two_run = run_spawn(root.path(), "two-mounts", &profile_config, &two)?;
    assert!(two_run.success, "{}", two_run.stderr);
    let mounts = mount_lines(&two_run.stdout);
    assert_eq!(mounts.len(), 2, "{}", two_run.stdout);
    assert!(mounts.iter().any(|line| line.ends_with(":/mnt/rw")));
    assert!(mounts.iter().any(|line| line.ends_with(":/mnt/ro:ro")));

    let missing = root.path().join("missing.json");
    write_json(
        &missing,
        &json!({"workspace": path_text(&workspace), "env": [], "agent_args": []}),
    )?;
    let missing_run = run_spawn(root.path(), "missing-mounts", &profile_config, &missing)?;
    assert!(missing_run.success, "{}", missing_run.stderr);
    assert!(
        mount_lines(&missing_run.stdout).is_empty(),
        "{}",
        missing_run.stdout
    );

    let empty = root.path().join("empty.json");
    write_json(
        &empty,
        &json!({"workspace": path_text(&workspace), "env": [], "agent_args": [], "mounts": []}),
    )?;
    let empty_run = run_spawn(root.path(), "empty-mounts", &profile_config, &empty)?;
    assert!(empty_run.success, "{}", empty_run.stderr);
    assert!(
        mount_lines(&empty_run.stdout).is_empty(),
        "{}",
        empty_run.stdout
    );

    Ok(())
}

#[test]
#[ignore = "child process receives per-test environment"]
fn spawn_config_child() -> TestResult {
    common::run_command_child()
}

fn run_spawn(
    root: &std::path::Path,
    label: &str,
    profile_config: &std::path::Path,
    spawn_config: &std::path::Path,
) -> TestResult<common::ChildRun> {
    common::run_child(
        "spawn_config_child",
        root,
        label,
        ChildSpec {
            command: Command::Spawn,
            profile_config: Some(profile_config.to_path_buf()),
            args: vec![
                String::from("--spawn-config"),
                spawn_config.display().to_string(),
                String::from("--stdio"),
            ],
            env: Vec::new(),
        },
    )
}

fn write_json(path: &std::path::Path, value: &Value) -> TestResult {
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(&value)?))?;
    Ok(())
}

fn mount_lines(output: &str) -> Vec<&str> {
    output
        .lines()
        .filter(|line| line.starts_with("MOUNT=-v "))
        .collect()
}

fn path_text(path: &std::path::Path) -> String {
    path.display().to_string()
}

fn write_profile_config_with_keys(root: &std::path::Path, path: &std::path::Path) -> TestResult {
    let key_dir = root.join("home/.ssh/deploy_keys");
    fs::create_dir_all(&key_dir)?;
    fs::write(key_dir.join("repo-key"), "private key\n")?;
    fs::write(key_dir.join("repo-key-signing"), "signing key\n")?;
    common::write_profile_config(
        path,
        &ProfileFixture {
            deploy_key: Some(String::from("repo-key")),
            ..ProfileFixture::default()
        },
    )?;
    Ok(())
}

const fn alternate_source_kind() -> &'static str {
    if cfg!(target_os = "macos") {
        "nix-descriptor"
    } else {
        "docker-archive"
    }
}
