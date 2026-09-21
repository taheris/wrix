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
