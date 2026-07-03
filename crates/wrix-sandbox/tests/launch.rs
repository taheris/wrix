mod common;

use std::{ffi::OsString, fs, path::Path};

use serde_json::json;
use wrix_sandbox::command::Command;

use common::{ChildSpec, ProfileFixture, TestResult};

#[test]
fn unsafe_podman_socket_env_is_ignored_without_explicit_opt_in() -> TestResult {
    assert_podman_api_socket_requires_explicit_unsafe_opt_in("unsafe-podman")
}

#[test]
fn podman_api_socket_requires_explicit_unsafe_opt_in() -> TestResult {
    assert_podman_api_socket_requires_explicit_unsafe_opt_in("podman-api")
}

#[test]
fn missing_key_env_paths_fail_before_container_start() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("missing-key-env")
        .tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    let key_dir = root.path().join("host-keys");
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&key_dir)?;
    common::write_profile_config(&profile_config, &deploy_key_fixture())?;

    let missing_deploy = key_dir.join("missing-deploy");
    let deploy_missing = run_launch(
        root.path(),
        "missing-deploy",
        &profile_config,
        &workspace,
        vec![(
            String::from("WRIX_DEPLOY_KEY"),
            missing_deploy.as_os_str().to_os_string(),
        )],
    )?;
    assert!(!deploy_missing.success);
    assert!(deploy_missing.stderr.contains(&format!(
        "WRIX_DEPLOY_KEY={}: file does not exist",
        missing_deploy.display()
    )));
    assert_no_launch_plan(&deploy_missing.stdout);

    let deploy_key = key_dir.join("repo-key");
    fs::write(&deploy_key, "private key\n")?;
    let missing_signing = key_dir.join("missing-signing");
    let signing_missing = run_launch(
        root.path(),
        "missing-signing",
        &profile_config,
        &workspace,
        vec![
            (
                String::from("WRIX_DEPLOY_KEY"),
                deploy_key.as_os_str().to_os_string(),
            ),
            (
                String::from("WRIX_SIGNING_KEY"),
                missing_signing.as_os_str().to_os_string(),
            ),
        ],
    )?;
    assert!(!signing_missing.success);
    assert!(signing_missing.stderr.contains(&format!(
        "WRIX_SIGNING_KEY={}: file does not exist",
        missing_signing.display()
    )));
    assert_no_launch_plan(&signing_missing.stdout);

    Ok(())
}

#[test]
fn spawn_requires_resolved_keys_but_run_allows_missing_keys() -> TestResult {
    let root = tempfile::Builder::new().prefix("spawn-keys").tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    let spawn_config = root.path().join("spawn.json");
    fs::create_dir_all(&workspace)?;
    common::write_profile_config(&profile_config, &deploy_key_fixture())?;
    write_spawn_config(&spawn_config, &workspace)?;

    let run = run_launch(
        root.path(),
        "run-without-keys",
        &profile_config,
        &workspace,
        Vec::new(),
    )?;
    assert!(run.success, "{}", run.stderr);
    assert!(!run.stdout.contains("ENV=WRIX_DEPLOY_KEY="));
    assert!(!run.stdout.contains("ENV=WRIX_SIGNING_KEY="));

    let missing_deploy = run_spawn_launch(
        root.path(),
        "spawn-missing-deploy",
        &profile_config,
        &spawn_config,
        Vec::new(),
    )?;
    assert!(!missing_deploy.success);
    assert!(
        missing_deploy
            .stderr
            .contains("wrix spawn: no deploy key resolved")
    );
    assert!(missing_deploy.stderr.contains("repo-key"));
    assert_no_launch_plan(&missing_deploy.stdout);

    let key_dir = root.path().join("home/.ssh/deploy_keys");
    fs::create_dir_all(&key_dir)?;
    fs::write(key_dir.join("repo-key"), "private key\n")?;
    let missing_signing = run_spawn_launch(
        root.path(),
        "spawn-missing-signing",
        &profile_config,
        &spawn_config,
        Vec::new(),
    )?;
    assert!(!missing_signing.success);
    assert!(
        missing_signing
            .stderr
            .contains("wrix spawn: no signing key resolved")
    );
    assert!(missing_signing.stderr.contains("repo-key-signing"));
    assert_no_launch_plan(&missing_signing.stdout);

    Ok(())
}

#[test]
fn deploy_key_mount_uses_container_key_dir_without_public_key() -> TestResult {
    let root = tempfile::Builder::new().prefix("deploy-key").tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    let key_dir = root.path().join("host-keys");
    let key_path = key_dir.join("repo-key");
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&key_dir)?;
    fs::write(&key_path, "private key\n")?;
    fs::write(key_dir.join("repo-key.pub"), "public key\n")?;
    common::write_profile_config(
        &profile_config,
        &ProfileFixture {
            deploy_key: Some(String::from("repo-key")),
            ..ProfileFixture::default()
        },
    )?;

    let run = run_launch(
        root.path(),
        "deploy-key",
        &profile_config,
        &workspace,
        vec![(
            String::from("WRIX_DEPLOY_KEY"),
            key_path.as_os_str().to_os_string(),
        )],
    )?;

    assert!(run.success, "{}", run.stderr);
    assert!(run.stdout.contains(":/etc/wrix/keys:ro"));
    assert!(
        run.stdout
            .contains("ENV=WRIX_DEPLOY_KEY=/etc/wrix/keys/repo-key")
    );
    assert!(!run.stdout.contains("repo-key.pub"));
    assert!(!run.stdout.contains(&key_path.display().to_string()));

    Ok(())
}

#[test]
fn linux_default_boundary_sets_is_sandbox_without_fakeuid() -> TestResult {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }

    let root = tempfile::Builder::new()
        .prefix("linux-boundary")
        .tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    fs::create_dir_all(&workspace)?;
    common::write_profile_config(&profile_config, &ProfileFixture::default())?;

    let run = run_launch(
        root.path(),
        "default-boundary",
        &profile_config,
        &workspace,
        Vec::new(),
    )?;
    assert!(run.success, "{}", run.stderr);
    assert!(run.stdout.contains("ENV=IS_SANDBOX=1"));
    assert!(!run.stdout.contains("LD_PRELOAD"));
    assert!(!run.stdout.contains("libfakeuid"));

    Ok(())
}

#[test]
#[ignore = "child process receives per-test environment"]
fn launch_child() -> TestResult {
    common::run_command_child()
}

fn run_launch(
    root: &Path,
    label: &str,
    profile_config: &Path,
    workspace: &Path,
    env: Vec<(String, OsString)>,
) -> TestResult<common::ChildRun> {
    common::run_child(
        "launch_child",
        root,
        label,
        ChildSpec {
            command: Command::Run,
            profile_config: Some(profile_config.to_path_buf()),
            args: vec![workspace.display().to_string(), String::from("true")],
            env,
        },
    )
}

fn run_spawn_launch(
    root: &Path,
    label: &str,
    profile_config: &Path,
    spawn_config: &Path,
    env: Vec<(String, OsString)>,
) -> TestResult<common::ChildRun> {
    common::run_child(
        "launch_child",
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
            env,
        },
    )
}

fn assert_podman_api_socket_requires_explicit_unsafe_opt_in(prefix: &str) -> TestResult {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }

    let root = tempfile::Builder::new().prefix(prefix).tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    fs::create_dir_all(&workspace)?;
    common::write_profile_config(&profile_config, &ProfileFixture::default())?;

    let default_run = run_launch(
        root.path(),
        "default",
        &profile_config,
        &workspace,
        Vec::new(),
    )?;
    assert!(default_run.success, "{}", default_run.stderr);
    assert_socket_absent(&default_run.stdout);

    let legacy = run_launch(
        root.path(),
        "legacy",
        &profile_config,
        &workspace,
        vec![(String::from("WRIX_PODMAN_SOCKET"), OsString::from("1"))],
    )?;
    assert!(legacy.success, "{}", legacy.stderr);
    assert_socket_absent(&legacy.stdout);

    let missing = run_launch(
        root.path(),
        "missing",
        &profile_config,
        &workspace,
        vec![(
            String::from("WRIX_UNSAFE_PODMAN_SOCKET"),
            OsString::from("1"),
        )],
    )?;
    assert!(!missing.success);
    assert!(
        missing
            .stderr
            .contains("WRIX_UNSAFE_PODMAN_SOCKET set but socket not found")
    );

    #[cfg(unix)]
    {
        use std::os::unix::net::UnixListener;

        let socket_dir = root.path().join("runtime/podman");
        fs::create_dir_all(&socket_dir)?;
        let socket_path = socket_dir.join("podman.sock");
        let _listener = UnixListener::bind(&socket_path)?;
        let opted_in = run_launch(
            root.path(),
            "opted-in",
            &profile_config,
            &workspace,
            vec![(
                String::from("WRIX_UNSAFE_PODMAN_SOCKET"),
                OsString::from("1"),
            )],
        )?;
        assert!(opted_in.success, "{}", opted_in.stderr);
        assert!(opted_in.stdout.contains(&format!(
            "MOUNT=-v {}:/run/podman/podman.sock",
            socket_path.display()
        )));
        assert!(
            opted_in
                .stdout
                .contains("ENV=CONTAINER_HOST=unix:///run/podman/podman.sock")
        );
        assert!(opted_in.stdout.contains("ENV=GC_HOST_WORKSPACE="));
    }

    Ok(())
}

fn assert_socket_absent(output: &str) {
    assert!(!output.contains("/run/podman/podman.sock"));
    assert!(!output.contains("CONTAINER_HOST"));
    assert!(!output.contains("GC_HOST_WORKSPACE"));
    assert!(!output.contains("GC_HOST_BEADS"));
}

fn assert_no_launch_plan(output: &str) {
    assert!(output.is_empty(), "{output}");
}

fn deploy_key_fixture() -> ProfileFixture {
    ProfileFixture {
        deploy_key: Some(String::from("repo-key")),
        ..ProfileFixture::default()
    }
}

fn write_spawn_config(path: &Path, workspace: &Path) -> TestResult {
    let value = json!({
        "workspace": workspace.display().to_string(),
        "env": [],
        "agent_args": ["true"],
        "mounts": []
    });
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(&value)?))?;
    Ok(())
}
