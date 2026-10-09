mod common;
#[path = "common/lifecycle/mod.rs"]
mod lifecycle;

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
fn git_grants_follow_override_repo_default_precedence() -> TestResult {
    for mode in [Command::Run, Command::Spawn] {
        let fixture = GrantFixture::new()?;
        fixture.init_repository()?;
        for overrides in [None, Some(false), Some(true)] {
            for sign_override in [None, Some(false), Some(true)] {
                for (deploy, sign) in grant_combinations() {
                    fs::write(
                        fixture.workspace.join("wrix.toml"),
                        format!(
                            "[wrix.git]\ndeploy_key = 'repo-key'\ndeploy = {deploy}\nsign = {sign}\n"
                        ),
                    )?;
                    let output = fixture.run(
                        mode,
                        GitGrants {
                            deploy: overrides,
                            sign: sign_override,
                        },
                        Vec::new(),
                        true,
                    )?;
                    assert!(output.success, "{}", output.stderr);
                    assert_key_environment(
                        &output.stdout,
                        overrides.unwrap_or(deploy),
                        sign_override.unwrap_or(sign),
                    );
                }
            }
        }
        fs::remove_file(fixture.workspace.join("wrix.toml"))?;
        let output = fixture.run(mode, GitGrants::default(), Vec::new(), true)?;
        assert!(output.success, "{}", output.stderr);
        assert_key_environment(&output.stdout, false, false);
    }
    Ok(())
}

#[test]
fn git_key_identity_uses_the_selected_repository_before_profile() -> TestResult {
    for mode in [Command::Run, Command::Spawn] {
        let mut fixture = GrantFixture::new()?;
        fixture.init_repository()?;
        common::write_profile_config(
            &fixture.profile,
            &ProfileFixture {
                deploy_key: Some(String::from("profile-key")),
                ..ProfileFixture::default()
            },
        )?;
        fs::write(
            fixture.workspace.join("wrix.toml"),
            "[wrix.git]\ndeploy_key = 'repo-key'\ndeploy = true\n",
        )?;
        fixture.workspace = fixture.workspace.join("src");
        fs::create_dir(&fixture.workspace)?;
        fs::write(
            fixture.workspace.join("wrix.toml"),
            "ignored non-root policy",
        )?;
        let output = fixture.run(mode, GitGrants::default(), Vec::new(), true)?;
        assert!(output.success, "{}", output.stderr);
        assert_key_environment(&output.stdout, true, false);

        fixture.workspace = fixture
            .workspace
            .parent()
            .ok_or("repository root missing")?
            .join(".loom/integration");
        fs::create_dir_all(&fixture.workspace)?;
        fixture.init_repository()?;
        fs::write(
            fixture.workspace.join("wrix.toml"),
            "[wrix.git]\ndeploy_key = 'repo-key'\nsign = true\n",
        )?;
        let output = fixture.run(mode, GitGrants::default(), Vec::new(), true)?;
        assert!(output.success, "{}", output.stderr);
        assert_key_environment(&output.stdout, false, true);
    }
    Ok(())
}

#[test]
fn git_key_grants_are_independent_and_ambient_sources_do_not_grant() -> TestResult {
    for mode in [Command::Run, Command::Spawn] {
        let fixture = GrantFixture::new()?;
        fs::write(
            fixture.workspace.join("wrix.toml"),
            "invalid policy outside a repository",
        )?;
        for ambient_valid in [false, true] {
            let ambient = fixture.pointer_environment(ambient_valid, ambient_valid);
            let output = fixture.run(mode, GitGrants::default(), ambient, true)?;
            assert!(output.success, "{}", output.stderr);
            assert_key_environment(&output.stdout, false, false);
        }
        for (deploy, sign) in grant_combinations() {
            for invalid_ungranted in [false, true] {
                for retired_sign in ["0", "1"] {
                    let mut environment = fixture.pointer_environment(
                        deploy || !invalid_ungranted,
                        sign || !invalid_ungranted,
                    );
                    environment.push((String::from("WRIX_GIT_SIGN"), OsString::from(retired_sign)));
                    let output = fixture.run(
                        mode,
                        GitGrants {
                            deploy: Some(deploy),
                            sign: Some(sign),
                        },
                        environment,
                        true,
                    )?;
                    assert!(output.success, "{}", output.stderr);
                    assert_key_environment(&output.stdout, deploy, sign);
                    assert!(!output.stdout.contains("WRIX_GIT_SIGN="));
                    assert!(!output.stdout.contains("host-source"));
                }
            }
        }
        fs::remove_dir_all(fixture.root.path().join("home/.ssh"))?;
        let output = fixture.run(mode, GitGrants::default(), Vec::new(), true)?;
        assert!(output.success, "{}", output.stderr);
        assert_key_environment(&output.stdout, false, false);
    }
    Ok(())
}

#[test]
fn granted_git_keys_are_required_before_startup() -> TestResult {
    for mode in [Command::Run, Command::Spawn] {
        for (deploy, sign) in [(true, false), (false, true), (true, true)] {
            for (name, granted, variable, key) in [
                ("deploy", deploy, "WRIX_DEPLOY_KEY", "repo-key"),
                ("signing", sign, "WRIX_SIGNING_KEY", "repo-key-signing"),
            ] {
                if !granted {
                    continue;
                }
                for explicit in [false, true] {
                    let fixture = GrantFixture::new()?;
                    fs::create_dir_all(fixture.workspace.join(".beads/dolt"))?;
                    let environment = if explicit {
                        fixture.pointer_environment(name != "deploy", name != "signing")
                    } else {
                        fs::remove_file(
                            fixture.root.path().join("home/.ssh/deploy_keys").join(key),
                        )?;
                        Vec::new()
                    };
                    let output = fixture.run(
                        mode,
                        GitGrants {
                            deploy: Some(deploy),
                            sign: Some(sign),
                        },
                        environment,
                        false,
                    )?;
                    assert!(!output.success);
                    if explicit {
                        assert!(output.stderr.contains(variable), "{}", output.stderr);
                        assert!(
                            output.stderr.contains("missing-host-source"),
                            "{}",
                            output.stderr
                        );
                    } else {
                        assert!(
                            output
                                .stderr
                                .contains(&format!("granted {name} key unresolved")),
                            "{}",
                            output.stderr
                        );
                        assert!(output.stderr.contains(key), "{}", output.stderr);
                    }
                    assert_no_launch_plan(&output.stdout);
                    assert!(!fixture.root.path().join("argv").exists());
                    assert!(!fixture.root.path().join("cache/wrix").exists());
                    assert!(!fixture.workspace.join(".wrix").exists());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn independent_git_grants_use_fixed_private_key_destinations() -> TestResult {
    for mode in [Command::Run, Command::Spawn] {
        for (deploy, sign) in grant_combinations() {
            let fixture = GrantFixture::new()?;
            let output = fixture.run(
                mode,
                GitGrants {
                    deploy: Some(deploy),
                    sign: Some(sign),
                },
                fixture.pointer_environment(true, true),
                false,
            )?;
            assert!(output.success, "{}", output.stderr);
            let argv = fixture.argv()?;
            let env = argv
                .iter()
                .filter_map(|arg| arg.strip_prefix("WRIX_"))
                .collect::<Vec<_>>();
            assert_eq!(
                env.iter()
                    .filter(|arg| arg.starts_with("DEPLOY_KEY="))
                    .copied()
                    .collect::<Vec<_>>(),
                if deploy {
                    vec!["DEPLOY_KEY=/etc/wrix/keys/repo-key"]
                } else {
                    vec![]
                }
            );
            assert_eq!(
                env.iter()
                    .filter(|arg| arg.starts_with("SIGNING_KEY="))
                    .copied()
                    .collect::<Vec<_>>(),
                if sign {
                    vec!["SIGNING_KEY=/etc/wrix/keys/repo-key-signing"]
                } else {
                    vec![]
                }
            );
            assert!(!env.iter().any(|arg| arg.starts_with("GIT_SIGN=")));
            assert!(
                !argv
                    .iter()
                    .any(|arg| arg.contains("host-source") || arg.contains(".pub"))
            );
            assert_eq!(
                argv.iter()
                    .filter(|arg| arg.ends_with(":/etc/wrix/keys:ro"))
                    .count(),
                usize::from(deploy || sign)
            );
            let captured = fixture.root.path().join("keys");
            let mut names = fs::read_dir(&captured)?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<Result<Vec<_>, _>>()?;
            names.sort();
            let mut expected = Vec::new();
            if deploy {
                expected.push(OsString::from("repo-key"));
                assert_eq!(fs::read(captured.join("repo-key"))?, b"deploy fixture\n");
            }
            if sign {
                expected.push(OsString::from("repo-key-signing"));
                assert_eq!(
                    fs::read(captured.join("repo-key-signing"))?,
                    b"signing fixture\n"
                );
            }
            assert_eq!(names, expected);
            for arg in &argv {
                if let Some(source) = arg.strip_suffix(":/etc/wrix/keys:ro") {
                    assert!(!Path::new(source).exists());
                }
            }
        }
    }
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
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(&workspace)
        .status()?;
    assert!(status.success());
    fs::write(workspace.join("wrix.toml"), "[wrix.git]\ndeploy = true\n")?;
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
fn retired_sign_environment_does_not_change_bootstrap_signing() -> TestResult {
    let root = tempfile::tempdir()?;
    let signing = root.path().join("signing-key");
    let output = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&signing)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for retired_sign in ["0", "1"] {
        let home = root.path().join(format!("home-{retired_sign}"));
        fs::create_dir(&home)?;
        let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/util/git-ssh-setup.sh");
        let output = std::process::Command::new("bash")
            .args([
                "-c",
                "set -euo pipefail; source \"$1\"; git config --global --get commit.gpgsign",
                "bootstrap-test",
            ])
            .arg(helper)
            .env("HOME", home)
            .env("WRIX_SIGNING_KEY", &signing)
            .env_remove("WRIX_DEPLOY_KEY")
            .env("WRIX_GIT_SIGN", retired_sign)
            .env("GIT_AUTHOR_NAME", "Wrix Test")
            .env("GIT_AUTHOR_EMAIL", "wrix@example.test")
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("GIT_CONFIG_COUNT")
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout)?.trim(), "true");
    }
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
fn runtime_mcp_selection_reaches_entrypoint() -> TestResult {
    for command in [Command::Run, Command::Spawn] {
        for selection in [None, Some(""), Some("all"), Some("alpha,beta")] {
            let fixture = lifecycle::Fixture::new()?;
            let mut child = std::process::Command::new(std::env::current_exe()?);
            child.args(["mcp_selection_child", "--exact", "--ignored"]);
            let mut environment = vec![
                (
                    "WRIX_TEST_LAUNCH_KIND",
                    if command == Command::Run {
                        "run"
                    } else {
                        "spawn"
                    },
                ),
                ("WRIX_MCP_TMUX_AUDIT", "/retired/audit.jsonl"),
                ("WRIX_MCP_TMUX_AUDIT_FULL", "/retired/audit"),
            ];
            if command == Command::Spawn {
                let mut config: serde_json::Value =
                    serde_json::from_slice(&fs::read(&fixture.spawn)?)?;
                config["env"] =
                    selection.map_or_else(|| json!([]), |value| json!([["WRIX_MCP", value]]));
                fs::write(&fixture.spawn, serde_json::to_vec(&config)?)?;
                environment.push(("WRIX_MCP", "host-not-selected"));
            } else if let Some(selection) = selection {
                environment.push(("WRIX_MCP", selection));
            }
            let argv = fixture.launched_argv(child, &environment)?;
            let forwarded: Vec<_> = argv
                .iter()
                .filter(|arg| arg.starts_with("WRIX_MCP="))
                .collect();
            assert_eq!(
                forwarded,
                selection
                    .map(|value| format!("WRIX_MCP={value}"))
                    .iter()
                    .collect::<Vec<_>>()
            );
            assert!(!argv.iter().any(|arg| arg.starts_with("WRIX_MCP_TMUX_")));
        }
    }
    Ok(())
}

#[test]
#[ignore = "child process dispatches the production launcher with runtime MCP inputs"]
fn mcp_selection_child() -> TestResult {
    let profile =
        PathBuf::from(std::env::var_os("WRIX_TEST_PROFILE_CONFIG").ok_or("profile missing")?);
    let spawn = std::env::var("WRIX_TEST_SPAWN_CONFIG")?;
    let command =
        Command::parse(&std::env::var("WRIX_TEST_LAUNCH_KIND")?).ok_or("command missing")?;
    let args = match command {
        Command::Spawn => vec![String::from("--spawn-config"), spawn],
        Command::Run => {
            let config: serde_json::Value = serde_json::from_slice(&fs::read(spawn)?)?;
            vec![
                config["workspace"]
                    .as_str()
                    .ok_or("workspace missing")?
                    .to_owned(),
            ]
        }
    };
    let code = wrix_sandbox::command::run(
        command,
        Some(profile),
        &args,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )?;
    assert_eq!(code, std::process::ExitCode::SUCCESS);
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
fn profile_mounts_expand_only_home_and_user_variables() -> TestResult {
    let (root, profile_config, workspace) = podman_socket_fixture("mount-expansion")?;
    let home = root.path().join("home");
    let repeated_home = PathBuf::from(format!("{}/{}", home.display(), home.display()));
    for path in [&home, &home.join("test-user/config"), &repeated_home] {
        fs::create_dir_all(path)?;
    }
    let mounts = [
        "~",
        "~/test-user/config",
        "$HOME/$USER/config",
        "$HOME/$HOME",
    ];
    common::write_profile_config(
        &profile_config,
        &ProfileFixture {
            mounts: mounts
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    json!({
                        "source": source, "dest": format!("/mnt/test{index}"), "mode": "rw"
                    })
                })
                .collect(),
            ..ProfileFixture::default()
        },
    )?;
    let run = run_launch(
        root.path(),
        "expanded",
        &profile_config,
        &workspace,
        vec![(String::from("USER"), OsString::from("test-user"))],
    )?;
    assert!(run.success, "{}", run.stderr);
    for index in 0..mounts.len() {
        assert!(
            run.stdout.contains(&format!("/mnt/test{index}")),
            "{}",
            run.stdout
        );
    }
    assert!(!run.stdout.contains("$HOME"));
    assert!(!run.stdout.contains("$USER"));
    Ok(())
}

#[test]
fn spawn_waits_for_container_completion() -> TestResult {
    for stdio in [false, true] {
        for exit_code in [0, 37] {
            let fixture = lifecycle::Fixture::new()?;
            let mut child = std::process::Command::new(std::env::current_exe()?);
            child.args(["spawn_lifecycle_child", "--exact", "--ignored"]);
            fixture.assert_foreground(child, stdio, exit_code)?;
        }
    }
    Ok(())
}

#[test]
#[ignore = "child process dispatches the real launcher with isolated environment"]
fn spawn_lifecycle_child() -> TestResult {
    let profile =
        PathBuf::from(std::env::var_os("WRIX_TEST_PROFILE_CONFIG").ok_or("profile missing")?);
    let spawn = std::env::var("WRIX_TEST_SPAWN_CONFIG")?;
    let mut args = vec![String::from("--spawn-config"), spawn];
    if std::env::var("WRIX_TEST_STDIO")? == "1" {
        args.push(String::from("--stdio"));
    }
    let code = wrix_sandbox::command::run(
        Command::Spawn,
        Some(profile),
        &args,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )?;
    for value in 0..=u8::MAX {
        if code == std::process::ExitCode::from(value) {
            std::process::exit(i32::from(value));
        }
    }
    Err("launcher returned an unsupported exit code".into())
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

fn assert_key_environment(output: &str, deploy: bool, sign: bool) {
    for (name, granted, path) in [
        ("WRIX_DEPLOY_KEY", deploy, "/etc/wrix/keys/repo-key"),
        ("WRIX_SIGNING_KEY", sign, "/etc/wrix/keys/repo-key-signing"),
    ] {
        let actual = output
            .lines()
            .filter(|line| line.starts_with(&format!("ENV={name}=")))
            .collect::<Vec<_>>();
        let expected = if granted {
            vec![format!("ENV={name}={path}")]
        } else {
            Vec::new()
        };
        assert_eq!(actual, expected, "{output}");
    }
}

const fn grant_combinations() -> [(bool, bool); 4] {
    [(false, false), (true, false), (false, true), (true, true)]
}

#[derive(Clone, Copy, Default)]
struct GitGrants {
    deploy: Option<bool>,
    sign: Option<bool>,
}

struct GrantFixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    profile: PathBuf,
}

impl GrantFixture {
    fn new() -> TestResult<Self> {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir()?;
        let workspace = root.path().join("workspace");
        let profile = root.path().join("profile.json");
        let bin = root.path().join("bin");
        let key_dir = root.path().join("home/.ssh/deploy_keys");
        for directory in [&workspace, &bin, &key_dir] {
            fs::create_dir_all(directory)?;
        }
        for (name, contents) in [
            ("repo-key", "deploy fixture\n"),
            ("repo-key-signing", "signing fixture\n"),
        ] {
            fs::write(key_dir.join(name), contents)?;
            fs::write(root.path().join(format!("host-source-{name}")), contents)?;
            fs::write(
                root.path().join(format!("host-source-{name}.pub")),
                "not private\n",
            )?;
        }
        for name in ["podman", "container", "route"] {
            let path = bin.join(name);
            fs::write(
                &path,
                include_str!("../../wrix-cli/tests/fixtures/launch-runtime.sh"),
            )?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
        common::write_profile_config(&profile, &deploy_key_fixture())?;
        Ok(Self {
            root,
            workspace,
            profile,
        })
    }

    fn init_repository(&self) -> TestResult {
        let output = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&self.workspace)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    fn pointer_environment(&self, deploy_valid: bool, sign_valid: bool) -> Vec<(String, OsString)> {
        [
            ("WRIX_DEPLOY_KEY", "repo-key", deploy_valid),
            ("WRIX_SIGNING_KEY", "repo-key-signing", sign_valid),
        ]
        .into_iter()
        .map(|(variable, name, valid)| {
            let prefix = if valid {
                "host-source"
            } else {
                "missing-host-source"
            };
            (
                variable.to_owned(),
                self.root
                    .path()
                    .join(format!("{prefix}-{name}"))
                    .into_os_string(),
            )
        })
        .chain([(String::from("WRIX_GIT_SIGN"), OsString::from("1"))])
        .collect()
    }

    fn run(
        &self,
        mode: Command,
        grants: GitGrants,
        environment: Vec<(String, OsString)>,
        dry_run: bool,
    ) -> TestResult<common::ChildRun> {
        let mut args = Vec::new();
        match mode {
            Command::Run => {
                if let Some(deploy) = grants.deploy {
                    args.push(
                        if deploy {
                            "--git-deploy"
                        } else {
                            "--no-git-deploy"
                        }
                        .to_owned(),
                    );
                }
                if let Some(sign) = grants.sign {
                    args.push(if sign { "--git-sign" } else { "--no-git-sign" }.to_owned());
                }
                args.push(self.workspace.display().to_string());
            }
            Command::Spawn => {
                let config = self.root.path().join("spawn.json");
                let mut git = serde_json::Map::new();
                if let Some(deploy) = grants.deploy {
                    git.insert("deploy".to_owned(), json!(deploy));
                }
                if let Some(sign) = grants.sign {
                    git.insert("sign".to_owned(), json!(sign));
                }
                fs::write(
                    &config,
                    serde_json::to_vec(&json!({
                        "workspace": self.workspace, "git": git,
                        "env": [["WRIX_DEPLOY_KEY", "/must/not/forward"], ["WRIX_SIGNING_KEY", "/must/not/forward"], ["WRIX_GIT_SIGN", "1"]],
                        "agent_args": [], "mounts": []
                    }))?,
                )?;
                args.extend([String::from("--spawn-config"), config.display().to_string()]);
            }
        }
        let path = std::env::join_paths(std::iter::once(self.root.path().join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").ok_or("PATH missing")?),
        ))?;
        let mut env = vec![
            (String::from("PATH"), path),
            (
                String::from("WRIX_IMAGE_KEEP_FILE"),
                self.root.path().join("mru.json").into_os_string(),
            ),
            (
                String::from("WRIX_TEST_DIGEST"),
                OsString::from(format!("sha256:{}", "a".repeat(64))),
            ),
            (
                String::from("WRIX_TEST_ARGV"),
                self.root.path().join("argv").into_os_string(),
            ),
            (
                String::from("WRIX_TEST_KEYS"),
                self.root.path().join("keys").into_os_string(),
            ),
        ];
        env.extend(environment);
        common::run_child(
            "launch_child",
            self.root.path(),
            "grant",
            ChildSpec {
                command: mode,
                profile_config: Some(self.profile.clone()),
                args,
                env,
                dry_run,
            },
        )
    }

    fn argv(&self) -> TestResult<Vec<String>> {
        let bytes = fs::read(self.root.path().join("argv"))?;
        bytes
            .strip_suffix(&[0])
            .ok_or("missing argv NUL")?
            .split(|byte| *byte == 0)
            .map(|arg| String::from_utf8(arg.to_vec()).map_err(Into::into))
            .collect()
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
