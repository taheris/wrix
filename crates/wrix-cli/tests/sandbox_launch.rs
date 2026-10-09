use std::{fs, path::PathBuf, process::Command};

use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    profile: PathBuf,
}

impl Fixture {
    fn new(network: Option<Value>) -> TestResult<Self> {
        let root = tempfile::Builder::new().prefix("network-mode").tempdir()?;
        let workspace = root.path().join("workspace");
        let profile = root.path().join("profile.json");
        fs::create_dir_all(workspace.join(".beads/dolt"))?;
        let source_kind = if cfg!(target_os = "macos") {
            "docker-archive"
        } else {
            "nix-descriptor"
        };
        let mut value = json!({
            "schema": 1,
            "system": "test",
            "profile": { "name": "base" },
            "image": {
                "ref": "localhost/wrix-test:latest",
                "source": "/missing/image-source",
                "source_kind": source_kind,
                "digest": format!("sha256:{}", "a".repeat(64))
            },
            "agent": { "kind": "direct" },
            "services": { "nix_cache": { "enable": false } }
        });
        if let Some(network) = network {
            value["network"] = network;
        }
        fs::write(&profile, serde_json::to_vec(&value)?)?;
        Ok(Self {
            root,
            workspace,
            profile,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wrix"));
        command
            .arg("--profile-config")
            .arg(&self.profile)
            .arg("run")
            .arg(&self.workspace)
            .arg("true")
            .env("HOME", self.root.path().join("home"))
            .env_remove("WRIX_NETWORK")
            .env_remove("WRIX_DRY_RUN")
            .env_remove("WRIX_DRY_RUN_SERVICES");
        command
    }

    fn forbid_subprocesses(&self, command: &mut Command) -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let bin = self.root.path().join("bin");
        fs::create_dir_all(&bin)?;
        for name in ["git", "bd", "podman", "container", "nix", "wrix"] {
            let path = bin.join(name);
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' '{name}' >>'{}'\nexit 91\n",
                    self.root.path().join("calls").display()
                ),
            )?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
        command.env("PATH", bin);
        Ok(())
    }
}

#[cfg(target_os = "linux")]
#[test]
fn launcher_stages_only_beads_config_and_metadata() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(None)?;
    let beads = fixture.workspace.join(".beads");
    fs::remove_dir(beads.join("dolt"))?;
    fs::write(beads.join("config.yaml"), "issue-prefix: wx\n")?;
    fs::write(beads.join("metadata.json"), "{\"backend\":\"dolt\"}\n")?;
    fs::write(beads.join("issues.jsonl"), "private issue\n")?;
    fs::write(beads.join("credentials"), "not client metadata\n")?;
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin)?;
    let podman = bin.join("podman");
    fs::write(
        &podman,
        r#"#!/usr/bin/env bash
set -euo pipefail
case "$1 $2" in
  'image inspect') printf '%s\n' "$WRIX_TEST_DIGEST" ;;
  run*)
    for arg in "$@"; do
      case "$arg" in
        *:/workspace/.beads)
          source="${arg%:/workspace/.beads}"
          cp -R "$source" "$WRIX_TEST_CAPTURE"
          printf '%s\n' "$source" > "$WRIX_TEST_CAPTURE.source"
          exit 0
          ;;
      esac
    done
    exit 91
    ;;
esac
"#,
    )?;
    fs::set_permissions(&podman, fs::Permissions::from_mode(0o755))?;
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))?;
    let capture = fixture.root.path().join("captured-beads");
    let output = fixture
        .command()
        .env("PATH", path)
        .env("WRIX_TEST_DIGEST", format!("sha256:{}", "a".repeat(64)))
        .env("WRIX_TEST_CAPTURE", &capture)
        .env("WRIX_IMAGE_KEEP_FILE", fixture.root.path().join("mru.json"))
        .env("WRIX_GIT_SIGN", "0")
        .env_remove("WRIX_MICROVM")
        .env_remove("WRIX_UNSAFE_PODMAN_SOCKET")
        .env_remove("WRIX_DEPLOY_KEY")
        .env_remove("WRIX_SIGNING_KEY")
        .env_remove("TMUX")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in ["config.yaml", "metadata.json"] {
        assert_eq!(fs::read(capture.join(name))?, fs::read(beads.join(name))?);
    }
    assert_eq!(fs::read_dir(&capture)?.count(), 2);
    let source = fs::read_to_string(capture.with_extension("source"))?;
    assert!(!std::path::Path::new(source.trim()).exists());
    assert!(beads.join("issues.jsonl").is_file());
    Ok(())
}

#[test]
fn invalid_network_mode_fails_before_service_or_container_start() -> TestResult {
    let fixture = Fixture::new(None)?;
    let mut command = fixture.command();
    fixture.forbid_subprocesses(&mut command)?;
    let output = command.env("WRIX_NETWORK", "lan").output()?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("WRIX_NETWORK must be 'open' or 'limit'"),
        "{stderr}"
    );
    assert!(!fixture.root.path().join("calls").exists(), "{stderr}");
    Ok(())
}

#[test]
fn profile_network_defaults_and_explicit_environment_precedence() -> TestResult {
    for (network, override_mode, expected) in [
        (None, None, "open"),
        (Some(json!({})), None, "open"),
        (Some(json!({"default_mode":"limit"})), None, "limit"),
        (Some(json!({"default_mode":"open"})), None, "open"),
        (Some(json!({"default_mode":"limit"})), Some("open"), "open"),
        (Some(json!({"default_mode":"open"})), Some("limit"), "limit"),
    ] {
        let fixture = Fixture::new(network)?;
        let mut command = fixture.command();
        command.env("WRIX_DRY_RUN", "1");
        if let Some(mode) = override_mode {
            command.env("WRIX_NETWORK", mode);
        }
        let output = command.output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8(output.stdout)?.contains(&format!("ENV=WRIX_NETWORK={expected}"))
        );
    }
    Ok(())
}

#[test]
fn malformed_profile_config_fails_before_subprocesses() -> TestResult {
    for (pointer, value) in [
        ("/schema", json!(2)),
        ("/schema", json!(true)),
        ("/profile/name", json!("bad profile")),
        ("/image/ref", json!("--all")),
        ("/image/source", json!("")),
        ("/image/source", json!("/path\0suffix")),
        ("/image/source_kind", json!(null)),
        ("/image/source_kind", json!("tarball")),
        ("/image/digest", json!("sha256:short")),
        ("/agent/kind", json!("unknown-agent")),
        ("/services/nix_cache/enable", json!("true")),
    ] {
        let fixture = Fixture::new(None)?;
        let mut config: Value = serde_json::from_slice(&fs::read(&fixture.profile)?)?;
        *config
            .pointer_mut(pointer)
            .ok_or("fixture pointer missing")? = value;
        fs::write(&fixture.profile, serde_json::to_vec(&config)?)?;
        let mut command = fixture.command();
        fixture.forbid_subprocesses(&mut command)?;
        let output = command.output()?;
        assert!(!output.status.success(), "accepted {pointer}: {config}");
        assert!(
            !fixture.root.path().join("calls").exists(),
            "{pointer}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[test]
fn duplicate_profile_config_fields_fail_at_the_json_boundary() -> TestResult {
    let fixture = Fixture::new(None)?;
    let content = fs::read_to_string(&fixture.profile)?;
    fs::write(
        &fixture.profile,
        format!("{{\"schema\":1,{}", &content[1..]),
    )?;
    let mut command = fixture.command();
    fixture.forbid_subprocesses(&mut command)?;
    let output = command.output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate field"));
    assert!(!fixture.root.path().join("calls").exists());
    Ok(())
}

#[test]
fn malformed_network_policy_fails_before_subprocesses_even_with_override() -> TestResult {
    for network in [
        json!(null),
        json!(false),
        json!("limit"),
        json!({"default_mode":false}),
        json!({"default_mode":null}),
        json!({"default_mode":"lan"}),
        json!({"ipv6":"enabled"}),
    ] {
        let fixture = Fixture::new(Some(network))?;
        let mut command = fixture.command();
        fixture.forbid_subprocesses(&mut command)?;
        let output = command.env("WRIX_NETWORK", "open").output()?;
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains("ProfileConfig"), "{stderr}");
        assert!(!fixture.root.path().join("calls").exists(), "{stderr}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
impl Fixture {
    fn focus_spawn(&self, reply: &str) -> TestResult<Command> {
        use std::os::unix::fs::PermissionsExt;
        fs::remove_dir(self.workspace.join(".beads/dolt"))?;
        let bin = self.root.path().join("bin");
        fs::create_dir(&bin)?;
        for (name, script) in [
            (
                "niri",
                r#"#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == 'msg -j focused-window' ]] || exit 91
printf '%s\n' "$WRIX_TEST_FOCUS"
"#,
            ),
            (
                "tmux",
                r#"#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == 'display-message -p #{session_name}:#{window_index}.#{pane_index}' ]] || exit 91
printf '%s\n' 'test:1.0'
"#,
            ),
            (
                "podman",
                include_str!("../../../tests/standalone/notify-runtime.sh"),
            ),
        ] {
            let path = bin.join(name);
            fs::write(&path, script)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
        let deploy_key = self.root.path().join("deploy-key");
        fs::write(&deploy_key, "fixture deploy key\n")?;
        let spawn_config = self.root.path().join("spawn.json");
        fs::write(
            &spawn_config,
            serde_json::to_vec(&json!({
                "workspace": self.workspace,
                "env": [], "agent_args": [], "mounts": []
            }))?,
        )?;
        let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_wrix"));
        command
            .arg("--profile-config")
            .arg(&self.profile)
            .args(["spawn", "--spawn-config"])
            .arg(spawn_config)
            .arg("--stdio")
            .env("PATH", path)
            .env("HOME", self.root.path().join("home"))
            .env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("TMUX", "fixture")
            .env("NO_COLOR", "1")
            .env("WRIX_TEST_FOCUS", reply)
            .env("WRIX_TEST_DIGEST", format!("sha256:{}", "a".repeat(64)))
            .env("WRIX_TEST_CAPTURE", self.root.path().join("session.json"))
            .env(
                "WRIX_NOTIFY_TEST_ENV_CAPTURE",
                self.root.path().join("env.json"),
            )
            .env(
                "WRIX_NOTIFY_TEST_SESSION_DIR",
                self.root.path().join("runtime/wrix/sessions"),
            )
            .env("WRIX_NOTIFY_TEST_CLIENT", "0")
            .env("WRIX_IMAGE_KEEP_FILE", self.root.path().join("mru.json"))
            .env("WRIX_GIT_SIGN", "0");
        for name in [
            "WRIX_NETWORK",
            "WRIX_DRY_RUN",
            "WRIX_DRY_RUN_SERVICES",
            "WRIX_MICROVM",
            "WRIX_UNSAFE_PODMAN_SOCKET",
            "WRIX_DEPLOY_KEY",
            "WRIX_SIGNING_KEY",
            "WRIX_FOCUS_TARGET",
            "WRIX_SESSION_ID",
        ] {
            command.env_remove(name);
        }
        command.env("WRIX_DEPLOY_KEY", deploy_key);
        Ok(command)
    }
}

#[cfg(target_os = "linux")]
#[test]
fn spawn_without_focused_window_keeps_stdout_clean_and_does_not_warn() -> TestResult {
    let fixture = Fixture::new(None)?;
    let output = fixture.focus_spawn("null")?.output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(response["command"], "get_state");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("focus target"));
    let record: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("session.json"))?)?;
    assert!(record["window_id"].is_null());
    assert_eq!(record["focus_target"], "test:1.0");
    assert!(record.get("session_id").is_none());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn spawn_focus_warnings_go_to_stderr_without_corrupting_rpc_stdout() -> TestResult {
    let fixture = Fixture::new(None)?;
    let output = fixture.focus_spawn("not JSON")?.output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(response["command"], "get_state");
    assert!(String::from_utf8_lossy(&output.stderr).contains("focus target returned invalid JSON"));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn spawn_registers_focused_window_from_niri_reply() -> TestResult {
    let fixture = Fixture::new(None)?;
    let output = fixture.focus_spawn(r#"{"id":42}"#)?.output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("session.json"))?)?;
    assert_eq!(record["window_id"], "42");
    assert_eq!(record["focus_target"], "test:1.0");
    assert_eq!(record["tmux_target"], "test:1.0");
    assert!(record.get("session_id").is_none());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn launcher_focus_handoff_is_opaque_optional_and_not_an_identity_alias() -> TestResult {
    for mode in ["run", "spawn"] {
        for target in [None, Some(""), Some(" host/é:opaque\n")] {
            let fixture = Fixture::new(None)?;
            let base = fixture.focus_spawn(r#"{"id":42}"#)?;
            let mut command = Command::new(base.get_program());
            command.envs(
                base.get_envs()
                    .filter_map(|(name, value)| value.map(|value| (name, value))),
            );
            for (name, value) in base.get_envs() {
                if value.is_none() {
                    command.env_remove(name);
                }
            }
            command.args(["--profile-config"]).arg(&fixture.profile);
            if mode == "run" {
                command.arg("run").arg(&fixture.workspace).arg("true");
            } else {
                command
                    .args(["spawn", "--spawn-config"])
                    .arg(fixture.root.path().join("spawn.json"));
            }
            command
                .env_remove("TMUX")
                .env("WRIX_SESSION_ID", "legacy:9.9")
                .env("PI_SESSION_ID", "conversation:9.9")
                .env("WRIX_EXECUTION_ID", "execution:9.9");
            if let Some(target) = target {
                command.env("WRIX_FOCUS_TARGET", target);
            }
            let output = command.output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let pairs: Vec<String> =
                serde_json::from_slice(&fs::read(fixture.root.path().join("env.json"))?)?;
            assert!(
                !pairs
                    .iter()
                    .any(|pair| pair.starts_with("WRIX_SESSION_ID="))
            );
            let focus_pairs: Vec<_> = pairs
                .iter()
                .filter(|pair| pair.starts_with("WRIX_FOCUS_TARGET="))
                .collect();
            let record: Value =
                serde_json::from_slice(&fs::read(fixture.root.path().join("session.json"))?)?;
            if let Some(target) = target.filter(|target| !target.is_empty()) {
                assert_eq!(focus_pairs, [&format!("WRIX_FOCUS_TARGET={target}")]);
                assert_eq!(record["focus_target"], target);
                assert!(record["tmux_target"].is_null());
            } else {
                assert_eq!(focus_pairs, Vec::<&String>::new());
                assert!(record.is_null());
            }
            let directory = fixture.root.path().join("runtime/wrix/sessions");
            if directory.exists() {
                assert!(fs::read_dir(directory)?.all(|entry| {
                    entry
                        .unwrap()
                        .path()
                        .extension()
                        .is_none_or(|ext| ext != "json")
                }));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn spawn_focus_override_registers_routing_separately_from_host_tmux() -> TestResult {
    for target in ["opaque host", ""] {
        let fixture = Fixture::new(None)?;
        let mut command = fixture.focus_spawn(r#"{"id":42}"#)?;
        fs::write(
            fixture.root.path().join("spawn.json"),
            serde_json::to_vec(&json!({
                "workspace": fixture.workspace,
                "env": [["WRIX_FOCUS_TARGET", target]],
                "agent_args": []
            }))?,
        )?;
        let output = command
            .env("WRIX_FOCUS_TARGET", "ambient overridden target")
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let record: Value =
            serde_json::from_slice(&fs::read(fixture.root.path().join("session.json"))?)?;
        if target.is_empty() {
            assert!(record.is_null());
        } else {
            assert_eq!(record["focus_target"], target);
            assert_eq!(record["tmux_target"], "test:1.0");
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn focus_fixtures_reject_unexpected_external_calls() -> TestResult {
    let fixture = Fixture::new(None)?;
    let _command = fixture.focus_spawn("null")?;
    for name in ["niri", "tmux", "podman"] {
        let output = Command::new(fixture.root.path().join("bin").join(name))
            .args(["unexpected", "arguments"])
            .env("WRIX_TEST_DIGEST", format!("sha256:{}", "a".repeat(64)))
            .output()?;
        assert_eq!(output.status.code(), Some(91), "{name}");
        assert!(output.stdout.is_empty(), "{name}");
    }
    Ok(())
}
