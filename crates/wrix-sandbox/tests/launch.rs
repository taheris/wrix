mod common;

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use serde_json::json;
use wrix_sandbox::command::Command;

use common::{ChildSpec, ProfileFixture, TestResult};

#[test]
fn default_launch_omits_host_podman_socket() -> TestResult {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let (root, profile_config, workspace) = podman_socket_fixture("podman-default")?;

    let run = run_launch(
        root.path(),
        "default",
        &profile_config,
        &workspace,
        Vec::new(),
    )?;

    assert!(run.success, "{}", run.stderr);
    assert_socket_absent(&run.stdout);
    Ok(())
}

#[test]
fn legacy_podman_socket_env_is_ignored() -> TestResult {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let (root, profile_config, workspace) = podman_socket_fixture("podman-legacy")?;

    let run = run_launch(
        root.path(),
        "legacy",
        &profile_config,
        &workspace,
        vec![(String::from("WRIX_PODMAN_SOCKET"), OsString::from("1"))],
    )?;

    assert!(run.success, "{}", run.stderr);
    assert_socket_absent(&run.stdout);
    Ok(())
}

#[test]
fn unsafe_podman_socket_opt_in_requires_existing_socket() -> TestResult {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let (root, profile_config, workspace) = podman_socket_fixture("podman-missing")?;

    let run = run_launch(
        root.path(),
        "missing",
        &profile_config,
        &workspace,
        vec![(
            String::from("WRIX_UNSAFE_PODMAN_SOCKET"),
            OsString::from("1"),
        )],
    )?;

    assert!(!run.success);
    assert!(
        run.stderr
            .contains("WRIX_UNSAFE_PODMAN_SOCKET set but socket not found")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn unsafe_podman_socket_opt_in_mounts_existing_socket() -> TestResult {
    use std::os::unix::net::UnixListener;

    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let (root, profile_config, workspace) = podman_socket_fixture("podman-opt-in")?;
    let socket_dir = root.path().join("runtime/podman");
    fs::create_dir_all(&socket_dir)?;
    let socket_path = socket_dir.join("podman.sock");
    let _listener = UnixListener::bind(&socket_path)?;

    let run = run_launch(
        root.path(),
        "opted-in",
        &profile_config,
        &workspace,
        vec![(
            String::from("WRIX_UNSAFE_PODMAN_SOCKET"),
            OsString::from("1"),
        )],
    )?;

    assert!(run.success, "{}", run.stderr);
    assert!(run.stdout.contains(&format!(
        "MOUNT=-v {}:/run/podman/podman.sock",
        socket_path.display()
    )));
    assert!(
        run.stdout
            .contains("ENV=CONTAINER_HOST=unix:///run/podman/podman.sock")
    );
    assert!(run.stdout.contains("ENV=GC_HOST_WORKSPACE="));
    Ok(())
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

    let signing_disabled = run_spawn_launch(
        root.path(),
        "spawn-signing-disabled",
        &profile_config,
        &spawn_config,
        vec![(String::from("WRIX_GIT_SIGN"), OsString::from("0"))],
    )?;
    assert!(signing_disabled.success, "{}", signing_disabled.stderr);

    Ok(())
}

#[test]
fn profile_config_rejects_unsafe_deploy_key_names_before_staging() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("unsafe-deploy-key-name")
        .tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    fs::create_dir_all(&workspace)?;

    for (index, name) in [
        ".",
        "..",
        "../escaped",
        "/tmp/escaped",
        "nested/key",
        "nested\\key",
        "two words",
        " repo-key",
    ]
    .into_iter()
    .enumerate()
    {
        common::write_profile_config(
            &profile_config,
            &ProfileFixture {
                deploy_key: Some(name.to_owned()),
                ..ProfileFixture::default()
            },
        )?;
        let run = run_launch(
            root.path(),
            &format!("unsafe-key-{index}"),
            &profile_config,
            &workspace,
            Vec::new(),
        )?;
        assert!(!run.success, "unsafe key name was accepted: {name}");
        assert!(run.stderr.contains("security.deploy_key"), "{}", run.stderr);
        assert_no_launch_plan(&run.stdout);
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn non_unicode_runtime_passthrough_fails_loudly() -> TestResult {
    use std::os::unix::ffi::OsStringExt;

    let root = tempfile::Builder::new()
        .prefix("non-unicode-runtime-env")
        .tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    fs::create_dir_all(&workspace)?;
    common::write_profile_config(&profile_config, &ProfileFixture::default())?;

    let run = run_launch(
        root.path(),
        "non-unicode-runtime-env",
        &profile_config,
        &workspace,
        vec![(
            String::from("WRIX_MCP"),
            OsString::from_vec(vec![b't', b'm', b'u', b'x', 0xff]),
        )],
    )?;

    assert!(!run.success);
    assert!(
        run.stderr
            .contains("runtime environment variable WRIX_MCP is not valid Unicode")
    );
    assert_no_launch_plan(&run.stdout);
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
fn host_provider_credentials_reach_run_environment() -> TestResult {
    let root = tempfile::Builder::new().prefix("provider-env").tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    fs::create_dir_all(&workspace)?;
    common::write_profile_config(&profile_config, &ProfileFixture::default())?;

    let run = run_launch(
        root.path(),
        "provider-env",
        &profile_config,
        &workspace,
        vec![
            (
                String::from("OPENAI_API_KEY"),
                OsString::from("openai-test"),
            ),
            (
                String::from("ANTHROPIC_API_KEY"),
                OsString::from("anthropic-test"),
            ),
        ],
    )?;

    assert!(run.success, "{}", run.stderr);
    assert!(run.stdout.contains("ENV=OPENAI_API_KEY=[REDACTED]"));
    assert!(run.stdout.contains("ENV=ANTHROPIC_API_KEY=[REDACTED]"));
    assert!(!run.stdout.contains("openai-test"));
    assert!(!run.stdout.contains("anthropic-test"));
    Ok(())
}

#[test]
fn declared_custom_runtime_secret_reaches_run_environment() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("custom-runtime-secret")
        .tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    fs::create_dir_all(&workspace)?;
    common::write_profile_config(
        &profile_config,
        &ProfileFixture {
            runtime_secrets: BTreeMap::from([(
                String::from("CUSTOM_PROVIDER_TOKEN"),
                String::from("optional"),
            )]),
            ..ProfileFixture::default()
        },
    )?;

    let run = run_launch(
        root.path(),
        "custom-runtime-secret",
        &profile_config,
        &workspace,
        vec![(
            String::from("CUSTOM_PROVIDER_TOKEN"),
            OsString::from("custom-secret-value"),
        )],
    )?;

    assert!(run.success, "{}", run.stderr);
    assert!(run.stdout.contains("ENV=CUSTOM_PROVIDER_TOKEN=[REDACTED]"));
    assert!(!run.stdout.contains("custom-secret-value"));
    Ok(())
}

#[test]
fn required_runtime_secret_fails_before_container_start() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("required-runtime-secret")
        .tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    fs::create_dir_all(&workspace)?;
    common::write_profile_config(
        &profile_config,
        &ProfileFixture {
            runtime_secrets: BTreeMap::from([(
                String::from("WRIX_REQUIRED_SECRET_TEST_VALUE"),
                String::from("required"),
            )]),
            ..ProfileFixture::default()
        },
    )?;

    let run = run_launch(
        root.path(),
        "required-runtime-secret",
        &profile_config,
        &workspace,
        Vec::new(),
    )?;

    assert!(!run.success);
    assert!(
        run.stderr
            .contains("required runtime secret WRIX_REQUIRED_SECRET_TEST_VALUE")
    );
    assert_no_launch_plan(&run.stdout);
    Ok(())
}

#[test]
fn runtime_mcp_host_configuration_reaches_entrypoint() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("runtime-mcp-env")
        .tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    fs::create_dir_all(&workspace)?;
    common::write_profile_config(&profile_config, &ProfileFixture::default())?;

    let run = run_launch(
        root.path(),
        "runtime-mcp-env",
        &profile_config,
        &workspace,
        vec![
            (String::from("WRIX_MCP"), OsString::from("tmux")),
            (
                String::from("WRIX_MCP_TMUX_AUDIT"),
                OsString::from("/workspace/audit.jsonl"),
            ),
            (
                String::from("WRIX_MCP_TMUX_AUDIT_FULL"),
                OsString::from("/workspace/audit"),
            ),
        ],
    )?;

    assert!(run.success, "{}", run.stderr);
    assert!(run.stdout.contains("ENV=WRIX_MCP=tmux"));
    assert!(
        run.stdout
            .contains("ENV=WRIX_MCP_TMUX_AUDIT=/workspace/audit.jsonl")
    );
    assert!(
        run.stdout
            .contains("ENV=WRIX_MCP_TMUX_AUDIT_FULL=/workspace/audit")
    );
    Ok(())
}

#[test]
fn absent_optional_profile_mount_is_skipped() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("optional-mount")
        .tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    let missing = root.path().join("missing-cache");
    fs::create_dir_all(&workspace)?;
    common::write_profile_config(
        &profile_config,
        &ProfileFixture {
            mounts: vec![json!({
                "source": missing.display().to_string(),
                "dest": "/home/wrix/.cache/example",
                "mode": "rw",
                "optional": true
            })],
            ..ProfileFixture::default()
        },
    )?;

    let run = run_launch(
        root.path(),
        "optional-mount",
        &profile_config,
        &workspace,
        Vec::new(),
    )?;

    assert!(run.success, "{}", run.stderr);
    assert!(!run.stdout.contains("/home/wrix/.cache/example"));
    Ok(())
}

#[test]
fn pi_auth_file_uses_platform_delivery_path() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("pi-auth-delivery")
        .tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    let auth = root.path().join("pi/auth.json");
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(auth.parent().ok_or("auth path has no parent")?)?;
    fs::write(&auth, "{}\n")?;
    fs::write(
        auth.parent()
            .ok_or("auth path has no parent")?
            .join("sibling"),
        "must not be mounted\n",
    )?;
    common::write_profile_config(
        &profile_config,
        &ProfileFixture {
            agent_kind: String::from("pi"),
            ..ProfileFixture::default()
        },
    )?;

    let run = run_launch(
        root.path(),
        "pi-auth-delivery",
        &profile_config,
        &workspace,
        vec![(
            String::from("WRIX_PI_AUTH_FILE"),
            auth.as_os_str().to_os_string(),
        )],
    )?;

    assert!(run.success, "{}", run.stderr);
    assert!(run.stdout.contains(&format!(
        "MOUNT=-v {}.wrix-auth:/mnt/wrix/pi-agent-auth",
        auth.display()
    )));
    assert!(
        run.stdout
            .contains("ENV=WRIX_PI_AUTH_JSON=/mnt/wrix/pi-agent-auth/auth.json")
    );
    assert!(!run.stdout.contains("sibling"));
    assert!(auth.is_symlink());
    assert_eq!(fs::read_to_string(&auth)?, "{}\n");
    Ok(())
}

#[test]
fn pi_default_auth_survives_new_repository_launch() -> TestResult {
    let root = tempfile::tempdir()?;
    let profile = root.path().join("profile.json");
    common::write_profile_config(
        &profile,
        &ProfileFixture {
            agent_kind: String::from("pi"),
            ..ProfileFixture::default()
        },
    )?;
    let auth = root.path().join("home/.pi/agent/auth.json");
    for (index, repo) in ["first-repo", "second-repo"].iter().enumerate() {
        let workspace = root.path().join(repo);
        fs::create_dir(&workspace)?;
        let run = run_launch(root.path(), repo, &profile, &workspace, Vec::new())?;
        assert!(run.success, "{}", run.stderr);
        assert!(run.stdout.contains(&format!(
            "{}.wrix-auth:/mnt/wrix/pi-agent-auth",
            auth.display()
        )));
        if index == 0 {
            assert_eq!(fs::read_to_string(&auth)?, "{}\n");
            fs::write(&auth, "fixture credentials")?;
        } else {
            assert_eq!(fs::read_to_string(&auth)?, "fixture credentials");
        }
    }
    Ok(())
}

#[test]
fn missing_pi_auth_override_fails_without_initializing_it() -> TestResult {
    let root = tempfile::tempdir()?;
    let profile = root.path().join("profile.json");
    common::write_profile_config(
        &profile,
        &ProfileFixture {
            agent_kind: String::from("pi"),
            ..ProfileFixture::default()
        },
    )?;
    let auth = root.path().join("missing.json");
    let run = run_launch(
        root.path(),
        "missing",
        &profile,
        root.path(),
        vec![(
            String::from("WRIX_PI_AUTH_FILE"),
            auth.clone().into_os_string(),
        )],
    )?;
    assert!(!run.success);
    assert!(run.stderr.contains("WRIX_PI_AUTH_FILE="));
    assert!(!auth.exists());
    Ok(())
}

#[test]
fn pi_spawn_requires_existing_credentials() -> TestResult {
    let root = tempfile::tempdir()?;
    let profile = root.path().join("profile.json");
    let config = root.path().join("spawn.json");
    let key = root.path().join("deploy-key");
    fs::write(&key, "fixture key")?;
    common::write_profile_config(
        &profile,
        &ProfileFixture {
            agent_kind: String::from("pi"),
            ..ProfileFixture::default()
        },
    )?;
    write_spawn_config(&config, root.path())?;
    let run = run_spawn_launch(
        root.path(),
        "spawn",
        &profile,
        &config,
        vec![
            (String::from("WRIX_DEPLOY_KEY"), key.into_os_string()),
            (String::from("WRIX_GIT_SIGN"), OsString::from("0")),
        ],
    )?;
    assert!(!run.success);
    assert!(run.stderr.contains("wrix spawn: Pi auth file not found"));
    assert!(!root.path().join("home/.pi/agent/auth.json").exists());
    Ok(())
}

#[test]
fn non_pi_launch_does_not_prepare_or_mount_pi_credentials() -> TestResult {
    let root = tempfile::tempdir()?;
    let profile = root.path().join("profile.json");
    common::write_profile_config(&profile, &ProfileFixture::default())?;
    let auth = root.path().join("selected.json");
    fs::write(&auth, "fixture credentials")?;
    let run = run_launch(
        root.path(),
        "direct",
        &profile,
        root.path(),
        vec![(
            String::from("WRIX_PI_AUTH_FILE"),
            auth.clone().into_os_string(),
        )],
    )?;
    assert!(run.success, "{}", run.stderr);
    assert!(!run.stdout.contains("pi-agent-auth"));
    assert!(!auth.is_symlink());
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
            dry_run: true,
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
            dry_run: true,
        },
    )
}

fn podman_socket_fixture(prefix: &str) -> TestResult<(tempfile::TempDir, PathBuf, PathBuf)> {
    let root = tempfile::Builder::new().prefix(prefix).tempdir()?;
    let workspace = root.path().join("workspace");
    let profile_config = root.path().join("profile.json");
    fs::create_dir_all(&workspace)?;
    common::write_profile_config(&profile_config, &ProfileFixture::default())?;
    Ok((root, profile_config, workspace))
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
