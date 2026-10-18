use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    profile: PathBuf,
    spawn: PathBuf,
}

impl Fixture {
    fn new(rust: bool) -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let workspace = root.path().join("workspace");
        let profile = root.path().join("profile.json");
        let spawn = root.path().join("spawn.json");
        fs::create_dir_all(&workspace)?;
        assert!(
            Command::new("git")
                .arg("init")
                .arg(&workspace)
                .output()?
                .status
                .success()
        );
        fs::write(
            &profile,
            serde_json::to_vec(&json!({
                "schema": 1,
                "profile": {"name": if rust { "rust" } else { "base" }},
                "image": {
                    "ref": "localhost/wrix-test:latest",
                    "source": "/missing/image-source",
                    "source_kind": if cfg!(target_os = "macos") { "docker-archive" } else { "nix-descriptor" },
                    "digest": format!("sha256:{}", "a".repeat(64))
                },
                "agent": {"kind": "direct"},
                "services": {"nix_cache": {"enable": true}}
            }))?,
        )?;
        let fixture = Self {
            root,
            workspace,
            profile,
            spawn,
        };
        fixture.spawn_choice(None)?;
        Ok(fixture)
    }

    fn command(&self, mode: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wrix"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path().join("home"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(&self.workspace)
            .arg("--profile-config")
            .arg(&self.profile)
            .arg(mode)
            .args(["--dry-run", "--json"]);
        if mode == "spawn" {
            command.arg("--spawn-config").arg(&self.spawn);
        }
        command
    }

    fn spawn_choice(&self, shared: Option<bool>) -> TestResult {
        let mut value = json!({"workspace": self.workspace, "env": [], "agent_args": []});
        if let Some(shared) = shared {
            value["tool_caches"] = json!({"shared": shared});
        }
        fs::write(&self.spawn, serde_json::to_vec(&value)?)?;
        Ok(())
    }

    fn policy(&self, shared: bool) -> TestResult {
        fs::write(
            self.workspace.join("wrix.toml"),
            format!("[tool_caches]\nshared = {shared}\n"),
        )?;
        Ok(())
    }
}

fn assert_mode(output: Output, expected: &str) -> TestResult<String> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)?;
    let report: Value = serde_json::from_str(&text)?;
    assert_eq!(report["plan"]["tool_cache_mode"], expected);
    Ok(text)
}

#[test]
fn help_distinguishes_tool_sharing_from_nix_cache() -> TestResult {
    for mode in ["run", "spawn"] {
        let output = Command::new(env!("CARGO_BIN_EXE_wrix"))
            .args([mode, "--help"])
            .output()?;
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout)?;
        for term in [
            "shared worker tool caches",
            "fresh private writable local tool caches",
            "Nix/Celler",
            "explicit launch choice > repository tool_caches.shared > shared default",
        ] {
            assert!(help.contains(term), "{mode}: missing {term}: {help}");
        }
        if mode == "run" {
            assert!(help.contains("--cache"));
            assert!(help.contains("--no-cache"));
        } else {
            assert!(help.contains("SpawnConfig.tool_caches.shared"));
            assert!(help.contains("optional boolean"));
        }
    }
    Ok(())
}

#[test]
fn run_cache_flags_follow_prefix_and_conflict_rules() -> TestResult {
    let fixture = Fixture::new(false)?;
    for (flags, expected) in [
        (vec![], "shared"),
        (vec!["--cache"], "shared"),
        (vec!["--cache", "--cache"], "shared"),
        (vec!["--no-cache"], "private"),
    ] {
        assert_mode(fixture.command("run").args(flags).output()?, expected)?;
    }
    for args in [
        vec!["--cache", "--no-cache"],
        vec!["--no-cache", "--cache"],
        vec!["--shared-tool-caches"],
        vec!["--no-shared-tool-caches"],
    ] {
        let output = fixture.command("run").args(args).output()?;
        assert!(!output.status.success());
        let report: Value = serde_json::from_slice(&output.stdout)?;
        assert!(report["plan"].is_null());
    }
    for prefix in [vec!["--"], vec!["."], vec![".", "--"]] {
        let text = assert_mode(
            fixture
                .command("run")
                .args(prefix)
                .args(["--no-cache", "--cache", "--shared-tool-caches"])
                .output()?,
            "shared",
        )?;
        for flag in ["--no-cache", "--cache", "--shared-tool-caches"] {
            assert!(!text.contains(flag), "{text}");
        }
    }
    for flag in ["--cache", "--no-cache"] {
        assert!(
            !fixture
                .command("spawn")
                .arg(flag)
                .output()?
                .status
                .success()
        );
    }
    assert!(!fixture.workspace.join("wrix.toml").exists());
    assert!(!fixture.workspace.join(".wrix").exists());
    Ok(())
}

#[test]
fn one_off_cache_choices_do_not_persist_policy() -> TestResult {
    let fixture = Fixture::new(false)?;
    for repo in [None, Some(false), Some(true)] {
        if let Some(repo) = repo {
            fixture.policy(repo)?;
        }
        let policy_path = fixture.workspace.join("wrix.toml");
        let original = if policy_path.exists() {
            Some(fs::read(&policy_path)?)
        } else {
            None
        };
        for explicit in [None, Some(false), Some(true)] {
            let expected = if explicit.or(repo).unwrap_or(true) {
                "shared"
            } else {
                "private"
            };
            let mut run = fixture.command("run");
            if let Some(shared) = explicit {
                run.arg(if shared { "--cache" } else { "--no-cache" });
            }
            assert_mode(run.output()?, expected)?;
            fixture.spawn_choice(explicit)?;
            assert_mode(fixture.command("spawn").output()?, expected)?;
            match &original {
                Some(bytes) => assert_eq!(&fs::read(&policy_path)?, bytes),
                None => assert!(!policy_path.exists()),
            }
            assert_mode(
                fixture.command("run").output()?,
                if repo == Some(false) {
                    "private"
                } else {
                    "shared"
                },
            )?;
        }
    }
    Ok(())
}

#[test]
fn restricted_rust_requires_effective_private_tool_caches() -> TestResult {
    for mode in ["run", "spawn"] {
        for repo in [None, Some(false), Some(true)] {
            for explicit in [None, Some(false), Some(true)] {
                let fixture = Fixture::new(true)?;
                if let Some(repo) = repo {
                    fixture.policy(repo)?;
                }
                fixture.spawn_choice(explicit)?;
                let mut command = fixture.command(mode);
                command.env("WRIX_NETWORK", "limit");
                if mode == "run"
                    && let Some(shared) = explicit
                {
                    command.arg(if shared { "--cache" } else { "--no-cache" });
                }
                let output = command.output()?;
                if explicit.or(repo).unwrap_or(true) {
                    assert!(!output.status.success());
                    let report: Value = serde_json::from_slice(&output.stdout)?;
                    assert_eq!(report["plan"]["tool_cache_mode"], "shared");
                    let error = report["findings"].to_string();
                    assert!(error.contains("--no-cache"), "{error}");
                    assert!(error.contains("Nix/Celler"), "{error}");
                    assert!(!fixture.workspace.join(".wrix").exists());
                    assert!(!fixture.root.path().join("cache").exists());
                    assert!(!fixture.root.path().join("runtime").exists());
                } else {
                    let text = assert_mode(output, "private")?;
                    assert!(!text.contains("SCCACHE_SERVER"));
                }
                let profile: Value = serde_json::from_slice(&fs::read(&fixture.profile)?)?;
                assert_eq!(profile["services"]["nix_cache"]["enable"], true);
                if let Some(repo) = repo {
                    assert_eq!(
                        fs::read_to_string(fixture.workspace.join("wrix.toml"))?,
                        format!("[tool_caches]\nshared = {repo}\n")
                    );
                } else {
                    assert!(!fixture.workspace.join("wrix.toml").exists());
                }
            }
        }
    }
    Ok(())
}
