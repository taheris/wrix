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
