use std::{fs, path::PathBuf, process::Command};

use serde_json::{Value, json};
use wrix_sandbox::command::PROJECT_PROFILE_CONFIG;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    profile: PathBuf,
    spawn: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace)?;
        let profile = root.path().join("profile.json");
        let spawn = root.path().join("spawn.json");
        fs::write(
            &profile,
            serde_json::to_vec(&json!({
                "schema": 1,
                "profile": {"name": "selected-project"},
                "image": {
                    "ref": "localhost/wrix-test:latest",
                    "source": "/missing/source",
                    "source_kind": if cfg!(target_os = "macos") { "docker-archive" } else { "nix-descriptor" },
                    "digest": format!("sha256:{}", "a".repeat(64))
                },
                "agent": {"kind": "direct"},
                "services": {"nix_cache": {"enable": false}}
            }))?,
        )?;
        fs::write(
            &spawn,
            serde_json::to_vec(&json!({
                "workspace": workspace, "env": [], "agent_args": []
            }))?,
        )?;
        Ok(Self {
            root,
            workspace,
            profile,
            spawn,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wrix"));
        command.current_dir(self.root.path()).env_clear();
        command.env("HOME", self.root.path().join("home"));
        command.env("PATH", "/nonexistent");
        command
    }

    fn launch(&self, command: &mut Command, mode: &str) {
        command.arg(mode);
        if mode == "spawn" {
            command.arg("--spawn-config").arg(&self.spawn);
        } else {
            command.arg(&self.workspace);
        }
    }

    fn runtime(&self, command: &mut Command) -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let bin = self.root.path().join("bin");
        fs::create_dir_all(&bin)?;
        for name in ["podman", "container", "route", "skopeo"] {
            let path = bin.join(name);
            fs::write(&path, include_str!("fixtures/launch-runtime.sh"))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
        command.env(
            "PATH",
            std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            )))?,
        );
        command.env("XDG_RUNTIME_DIR", self.root.path().join("runtime"));
        command.env("XDG_CACHE_HOME", self.root.path().join("cache"));
        command.env("WRIX_TEST_DIGEST", format!("sha256:{}", "a".repeat(64)));
        command.env("WRIX_TEST_ARGV", self.root.path().join("argv"));
        let seccomp = self.root.path().join("worker-seccomp.json");
        fs::write(
            &seccomp,
            include_str!("../../wrix-sandbox/tests/fixtures/seccomp.json"),
        )?;
        command.env("WRIX_WORKER_SECCOMP", seccomp);
        Ok(())
    }
}

#[test]
fn explicit_profile_config_overrides_project_default() -> TestResult {
    for mode in ["run", "spawn"] {
        let fixture = Fixture::new()?;
        let mut command = fixture.command();
        fixture.runtime(&mut command)?;
        command.env(
            PROJECT_PROFILE_CONFIG,
            fixture.root.path().join("invalid.json"),
        );
        command.arg("--profile-config").arg(&fixture.profile);
        fixture.launch(&mut command, mode);
        let output = command.output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let argv = fs::read(fixture.root.path().join("argv"))?;
        assert!(String::from_utf8(argv)?.contains("localhost/wrix-test:latest"));
    }
    Ok(())
}

#[test]
fn generated_project_default_reaches_actual_run_and_spawn() -> TestResult {
    for mode in ["run", "spawn"] {
        let fixture = Fixture::new()?;
        let mut command = fixture.command();
        fixture.runtime(&mut command)?;
        command.env(PROJECT_PROFILE_CONFIG, &fixture.profile);
        fixture.launch(&mut command, mode);
        let output = command.output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(fixture.root.path().join("argv").exists());
    }
    Ok(())
}

#[test]
fn inspection_and_launch_share_project_configuration_resolution() -> TestResult {
    for mode in ["run", "spawn"] {
        for explicit in [false, true] {
            let fixture = Fixture::new()?;
            let mut command = fixture.command();
            fixture.runtime(&mut command)?;
            command.env(
                PROJECT_PROFILE_CONFIG,
                if explicit {
                    fixture.root.path().join("missing.json")
                } else {
                    fixture.profile.clone()
                },
            );
            if explicit {
                command.arg("--profile-config").arg(&fixture.profile);
            }
            command.arg(mode).args(["--dry-run", "--json"]);
            if mode == "spawn" {
                command.arg("--spawn-config").arg(&fixture.spawn);
            } else {
                command.arg(&fixture.workspace);
            }
            let preview = command.output()?;
            assert!(
                preview.status.success(),
                "{}",
                String::from_utf8_lossy(&preview.stderr)
            );
            let summary: Value = serde_json::from_slice(&preview.stdout)?;
            assert_eq!(summary["plan"]["profile"], "selected-project");
            assert_eq!(
                summary["plan"]["profile_config"],
                fixture.profile.display().to_string()
            );
            assert!(!fixture.root.path().join("argv").exists());
            let mut command = fixture.command();
            fixture.runtime(&mut command)?;
            command.env(
                PROJECT_PROFILE_CONFIG,
                if explicit {
                    fixture.root.path().join("missing.json")
                } else {
                    fixture.profile.clone()
                },
            );
            if explicit {
                command.arg("--profile-config").arg(&fixture.profile);
            }
            fixture.launch(&mut command, mode);
            let launch = command.output()?;
            assert!(
                launch.status.success(),
                "{}",
                String::from_utf8_lossy(&launch.stderr)
            );
            assert!(
                String::from_utf8(fs::read(fixture.root.path().join("argv"))?)?
                    .contains("localhost/wrix-test:latest")
            );
        }
    }
    Ok(())
}

#[test]
fn missing_or_invalid_launch_configuration_has_no_fallback() -> TestResult {
    for mode in ["run", "spawn"] {
        for input in [None, Some(""), Some("missing.json"), Some("malformed.json")] {
            let fixture = Fixture::new()?;
            fs::write(fixture.root.path().join("malformed.json"), "{")?;
            let mut command = fixture.command();
            if let Some(input) = input {
                command.env(PROJECT_PROFILE_CONFIG, input);
            }
            fixture.launch(&mut command, mode);
            let output = command.output()?;
            assert!(!output.status.success());
            let stderr = String::from_utf8(output.stderr)?;
            assert!(
                stderr.contains("ProfileConfig") || stderr.contains("profile config"),
                "{stderr}"
            );
            assert!(stderr.contains("--profile-config"), "{stderr}");
            assert!(stderr.contains("Home Manager"), "{stderr}");
            assert!(!fixture.root.path().join("home").exists());
            assert!(!fixture.root.path().join("runtime").exists());
        }
    }
    Ok(())
}

#[test]
fn invalid_explicit_configuration_never_falls_back_to_valid_project_data() -> TestResult {
    for mode in ["run", "spawn"] {
        let fixture = Fixture::new()?;
        let mut command = fixture.command();
        command.env(PROJECT_PROFILE_CONFIG, &fixture.profile);
        command.args(["--profile-config", "missing.json"]);
        fixture.launch(&mut command, mode);
        let output = command.output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)?.contains("profile config not found"));
        assert!(!fixture.root.path().join("home").exists());
    }
    Ok(())
}

#[test]
fn unsupported_project_configuration_fails_before_preparation() -> TestResult {
    for mode in ["run", "spawn"] {
        let fixture = Fixture::new()?;
        let mut profile: Value = serde_json::from_slice(&fs::read(&fixture.profile)?)?;
        profile["schema"] = json!(2);
        fs::write(&fixture.profile, serde_json::to_vec(&profile)?)?;
        for explicit in [false, true] {
            let mut command = fixture.command();
            command.env(PROJECT_PROFILE_CONFIG, &fixture.profile);
            if explicit {
                command.arg("--profile-config").arg(&fixture.profile);
            }
            fixture.launch(&mut command, mode);
            let output = command.output()?;
            assert!(!output.status.success());
            let stderr = String::from_utf8(output.stderr)?;
            assert!(
                stderr.contains("unsupported ProfileConfig schema"),
                "{stderr}"
            );
            assert!(!fixture.root.path().join("home").exists());
        }
    }
    Ok(())
}

#[test]
fn project_launch_defaults_do_not_configure_service_or_helper_commands() -> TestResult {
    let fixture = Fixture::new()?;
    for args in [
        vec!["service", "--help"],
        vec!["beads", "--help"],
        vec!["init", "--help"],
        vec!["service"],
        vec!["beads"],
        vec!["init", "--offline", "--no-hooks"],
    ] {
        let baseline = fixture.command().args(&args).output()?;
        let selected = fixture
            .command()
            .env(PROJECT_PROFILE_CONFIG, "missing.json")
            .args(&args)
            .output()?;
        assert_eq!(baseline.status.code(), selected.status.code());
        assert_eq!(baseline.stdout, selected.stdout);
        assert_eq!(baseline.stderr, selected.stderr);
        let explicit = fixture
            .command()
            .args(["--profile-config", "missing.json"])
            .args(&args)
            .output()?;
        assert!(!explicit.status.success());
        assert!(String::from_utf8(explicit.stderr)?.contains("only valid for run and spawn"));
    }
    assert!(!fixture.root.path().join("home").exists());
    Ok(())
}

#[test]
fn help_explains_host_cli_project_data_and_explicit_installation() -> TestResult {
    let fixture = Fixture::new()?;
    for args in [
        vec!["--help"],
        vec!["run", "--help"],
        vec!["spawn", "--help"],
    ] {
        let output = fixture.command().args(args).output()?;
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout)?;
        for expected in [
            "Home Manager",
            "nix-darwin",
            "nix profile add",
            "nix shell",
            "sandbox.devShell",
            "overrides",
            "run/spawn only",
            "No fixed installation directory",
        ] {
            assert!(help.contains(expected), "missing {expected}: {help}");
        }
        assert!(!help.contains("#sandbox-"));
        assert!(!help.contains("wrix-run"));
    }
    Ok(())
}
