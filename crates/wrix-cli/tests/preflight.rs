use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
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
    fn new() -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&workspace)?;
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .arg(&workspace)
                .status()?
                .success()
        );
        let profile = root.path().join("profile.json");
        let spawn = root.path().join("spawn.json");
        fs::write(
            &profile,
            serde_json::to_vec(&json!({
                "schema": 1, "profile": {"name": "base"},
                "image": {"ref": "localhost/wrix-test:latest", "source": "/missing/source", "source_kind": if cfg!(target_os = "macos") { "docker-archive" } else { "nix-descriptor" }, "digest": format!("sha256:{}", "a".repeat(64))},
                "agent": {"kind": "direct"},
                "security": {"runtime_secrets": {"ANTHROPIC_API_KEY": "optional", "CLAUDE_CODE_OAUTH_TOKEN": "optional", "OPENAI_API_KEY": "optional", "CUSTOM_TOKEN": "optional"}},
                "services": {"nix_cache": {"enable": false}}
            }))?,
        )?;
        fs::write(
            &spawn,
            serde_json::to_vec(&json!({"workspace": workspace, "env": [], "agent_args": []}))?,
        )?;
        Ok(Self {
            root,
            workspace,
            profile,
            spawn,
        })
    }

    fn change_profile(&self, change: impl FnOnce(&mut Value)) -> TestResult {
        let mut value = serde_json::from_slice(&fs::read(&self.profile)?)?;
        change(&mut value);
        fs::write(&self.profile, serde_json::to_vec(&value)?)?;
        Ok(())
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
            .arg(mode);
        if mode == "spawn" {
            command.arg("--spawn-config").arg(&self.spawn);
        }
        command
    }

    fn preview(&self, mode: &str, json: bool) -> Command {
        let mut command = self.command(mode);
        command.arg("--dry-run");
        if json {
            command.arg("--json");
        }
        command
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
        command
            .env(
                "PATH",
                std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
                    &std::env::var_os("PATH").unwrap_or_default(),
                )))?,
            )
            .env("WRIX_TEST_DIGEST", format!("sha256:{}", "a".repeat(64)))
            .env("WRIX_TEST_ARGV", self.root.path().join("argv"));
        Ok(())
    }
}

fn report(output: &Output) -> TestResult<Value> {
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn tree(root: &Path) -> TestResult<BTreeMap<PathBuf, Vec<u8>>> {
    let mut entries = BTreeMap::new();
    if !root.exists() {
        return Ok(entries);
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            entries.extend(tree(&path)?);
        } else {
            entries.insert(path.clone(), fs::read(path)?);
        }
    }
    Ok(entries)
}

#[test]
fn dry_run_renderings_share_a_versioned_report() -> TestResult {
    let fixture = Fixture::new()?;
    for blocker in [false, true] {
        fixture.change_profile(|profile| {
            profile["security"]["runtime_secrets"]["CUSTOM_TOKEN"] =
                json!(if blocker { "required" } else { "optional" });
        })?;
        let json = fixture.preview("run", true).output()?;
        let value = report(&json)?;
        let human = fixture.preview("run", false).output()?;
        assert_eq!(json.status.success(), !blocker);
        assert_eq!(json.status, human.status);
        assert_eq!(value["schema"], 1);
        let text = String::from_utf8(human.stdout)?;
        for finding in value["findings"].as_array().unwrap() {
            assert!(text.contains(finding["code"].as_str().unwrap()));
            assert!(text.contains(finding["message"].as_str().unwrap()));
        }
        for (key, value) in value["plan"].as_object().unwrap() {
            assert!(text.contains(&format!("{key}: {value}")), "{key}: {text}");
        }
    }
    Ok(())
}

#[test]
fn dry_run_json_is_only_a_preview_rendering_option() -> TestResult {
    let fixture = Fixture::new()?;
    for mode in ["run", "spawn"] {
        let output = fixture.command(mode).arg("--json").output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)?.contains("--json requires --dry-run"));
    }
    let mut command = fixture.command("run");
    fixture.runtime(&mut command)?;
    let output = command.args(["--", "--json"]).output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fs::read(fixture.root.path().join("argv"))?.ends_with(b"--json\0"));
    Ok(())
}

#[test]
fn dry_run_parses_explicit_launch_options() -> TestResult {
    let fixture = Fixture::new()?;
    for mode in ["run", "spawn"] {
        let mut command = fixture.preview(mode, true);
        if mode == "spawn" {
            command.arg("--stdio");
        }
        assert!(command.output()?.status.success());
        assert!(!fixture.root.path().join("argv").exists());
    }
    for args in [vec!["--", "--dry-run"], vec![".", "--", "--dry-run"]] {
        let mut command = fixture.command("run");
        fixture.runtime(&mut command)?;
        let output = command.args(args).output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(fs::read(fixture.root.path().join("argv"))?.ends_with(b"--dry-run\0"));
    }
    Ok(())
}

#[test]
fn dry_run_ignores_retired_environment_switches() -> TestResult {
    let fixture = Fixture::new()?;
    let mut command = fixture.command("run");
    fixture.runtime(&mut command)?;
    let output = command
        .env("WRIX_DRY_RUN", "1")
        .env("WRIX_DRY_RUN_SERVICES", "1")
        .env("WRIX_DRY_RUN_SERVICE_BIN", "/missing/forbidden-service")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fixture.root.path().join("argv").exists());
    assert!(fixture.workspace.join(".wrix/log").exists());
    Ok(())
}

#[test]
fn dry_run_reports_launch_policy_and_provenance() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.workspace.join("wrix.toml"),
        "[git]\ndeploy = true\nsign = false\n",
    )?;
    fixture.change_profile(|profile| {
        profile["resources"] = json!({"cpus": 2, "memory_mb": 1234, "pids_limit": 99});
        profile["profile"]["network_allowlist"] = json!([{"host": "example.org", "ports": [443]}]);
    })?;
    for mode in ["run", "spawn"] {
        let output = fixture
            .preview(mode, true)
            .env("WRIX_DEPLOY_KEY", &fixture.profile)
            .output()?;
        assert!(output.status.success());
        let value = report(&output)?;
        assert_eq!(
            value["plan"]["git_deploy"],
            json!({"enabled": true, "origin": "repository"})
        );
        assert_eq!(
            value["plan"]["git_sign"],
            json!({"enabled": false, "origin": "repository"})
        );
        assert_eq!(
            value["plan"]["workspace"],
            fixture.workspace.to_str().unwrap()
        );
        assert_eq!(value["plan"]["image_source"], "/missing/source");
        assert_eq!(value["plan"]["agent"], "direct");
        assert_eq!(
            value["plan"]["allowlist"],
            json!([{"host": "example.org", "ports": [443]}])
        );
    }
    let mut preview = fixture.preview("run", true);
    preview
        .args(["--no-git-deploy", "--git-sign"])
        .env("WRIX_SIGNING_KEY", &fixture.profile);
    let output = preview.output()?;
    assert!(output.status.success());
    let plan = &report(&output)?["plan"];
    assert_eq!(
        plan["git_deploy"],
        json!({"enabled": false, "origin": "invocation"})
    );
    assert_eq!(
        plan["git_sign"],
        json!({"enabled": true, "origin": "invocation"})
    );
    assert_eq!(plan["cpus"], 2);
    assert_eq!(plan["memory_mb"], 1234);
    assert_eq!(plan["pids_limit"], 99);
    assert_eq!(plan["network"], "open");
    let mut launch = fixture.command("run");
    fixture.runtime(&mut launch)?;
    let output = launch
        .args(["--no-git-deploy", "--git-sign"])
        .env("WRIX_SIGNING_KEY", &fixture.profile)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let argv = String::from_utf8(fs::read(fixture.root.path().join("argv"))?)?;
    for expected in [
        "localhost/wrix-test:latest",
        "WRIX_EFFECTIVE_GIT_SIGN=1",
        "WRIX_NETWORK=open",
    ] {
        assert!(argv.contains(expected), "{expected}: {argv}");
    }
    let arguments: Vec<_> = argv.split('\0').collect();
    let resources = if cfg!(target_os = "macos") {
        &["-c2", "-m1234M"][..]
    } else {
        &["--cpus=2", "--memory=1234m", "--pids-limit=99"][..]
    };
    for expected in resources {
        assert!(arguments.contains(expected), "{expected}: {argv}");
    }
    if cfg!(target_os = "macos") {
        assert!(!argv.contains("--pids-limit"));
    }
    assert!(!argv.contains("WRIX_DEPLOY_KEY="));
    Ok(())
}

#[test]
fn dry_run_reports_credential_availability_without_values() -> TestResult {
    let fixture = Fixture::new()?;
    for json in [false, true] {
        let output = fixture
            .preview("run", json)
            .env("CUSTOM_TOKEN", "confidential-payload")
            .output()?;
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout)?;
        assert!(text.contains("CUSTOM_TOKEN"));
        assert!(text.contains("host_environment"));
        assert!(text.contains("\"available\":true"));
        assert!(!text.contains("confidential-payload"));
    }
    Ok(())
}

#[test]
fn dry_run_is_read_only() -> TestResult {
    let fixture = Fixture::new()?;
    let auth = fixture.root.path().join("auth.json");
    fs::write(&auth, b"credential bytes are opaque\xff")?;
    let directory = fixture.root.path().join("mount-directory");
    fs::create_dir(&directory)?;
    fs::write(directory.join("sentinel"), b"must not be copied or changed")?;
    fs::create_dir(fixture.workspace.join(".beads"))?;
    fs::write(
        fixture.workspace.join(".beads/config.yaml"),
        "backend: dolt\n",
    )?;
    fs::write(
        fixture.workspace.join("wrix.toml"),
        "[git]\ndeploy = true\nsign = true\n",
    )?;
    fixture.change_profile(|profile| {
        profile["agent"]["kind"] = json!("pi");
        profile["services"]["nix_cache"]["enable"] = json!(true);
        profile["profile"]["mounts"] = json!([
            {"source": auth, "dest": "/test", "mode": "ro"},
            {"source": directory, "dest": "/directory", "mode": "ro"}
        ]);
    })?;
    for mode in ["run", "spawn"] {
        for json in [false, true] {
            let before = tree(fixture.root.path())?;
            let output = fixture
                .preview(mode, json)
                .env("WRIX_PI_AUTH_FILE", &auth)
                .env("WRIX_DEPLOY_KEY", &auth)
                .env("WRIX_SIGNING_KEY", &auth)
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(before, tree(fixture.root.path())?);
            for name in ["cache", "runtime", "home", "argv", "auth.json.wrix-auth"] {
                assert!(!fixture.root.path().join(name).exists(), "{name}");
            }
            assert!(!fixture.workspace.join(".wrix").exists());
        }
    }
    Ok(())
}

#[test]
fn preview_reports_effective_tool_cache_mode_without_preparation() -> TestResult {
    let fixture = Fixture::new()?;
    for shared in [false, true] {
        fs::write(
            fixture.workspace.join("wrix.toml"),
            format!("[tool_caches]\nshared = {shared}\n"),
        )?;
        for mode in ["run", "spawn"] {
            for json in [false, true] {
                let before = tree(fixture.root.path())?;
                let output = fixture.preview(mode, json).output()?;
                assert!(output.status.success());
                let text = String::from_utf8(output.stdout)?;
                assert!(text.contains(if shared { "shared" } else { "private" }));
                assert_eq!(before, tree(fixture.root.path())?);
            }
        }
    }
    Ok(())
}

#[test]
fn restricted_rust_cache_conflict_is_read_only_and_actionable() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.change_profile(|profile| profile["profile"]["name"] = json!("rust"))?;
    for mode in ["run", "spawn"] {
        for json in [false, true] {
            let before = tree(fixture.root.path())?;
            let output = fixture
                .preview(mode, json)
                .env("WRIX_NETWORK", "limit")
                .output()?;
            assert!(!output.status.success());
            let text = String::from_utf8(output.stdout)?;
            for expected in [
                "restricted_rust_shared_caches",
                "--no-cache",
                "rust",
                "limit",
                "shared",
            ] {
                assert!(text.contains(expected), "{text}");
            }
            assert_eq!(before, tree(fixture.root.path())?);
        }
    }
    Ok(())
}

#[test]
fn dry_run_summaries_do_not_dump_sensitive_inputs() -> TestResult {
    let fixture = Fixture::new()?;
    let sentinel = "private-sentinel-that-must-not-appear";
    fs::write(
        &fixture.spawn,
        serde_json::to_vec(
            &json!({"workspace": fixture.workspace, "env": [["NON_SECRET", sentinel]], "agent_args": [sentinel], "extensions": {"private": sentinel}}),
        )?,
    )?;
    let valid_profile = fs::read(&fixture.profile)?;
    for json in [false, true] {
        fs::write(&fixture.profile, &valid_profile)?;
        let output = fixture.preview("spawn", json).output()?;
        assert!(output.status.success());
        assert!(!String::from_utf8(output.stdout)?.contains(sentinel));
        fs::write(&fixture.profile, format!("{{\"schema\":\"{sentinel}\"}}"))?;
        let output = fixture.preview("spawn", json).output()?;
        assert!(!output.status.success());
        assert_eq!(output.stderr, b"");
        assert!(!String::from_utf8(output.stdout)?.contains(sentinel));
    }
    Ok(())
}

#[test]
fn dry_run_redacts_builtin_provider_credentials() -> TestResult {
    let fixture = Fixture::new()?;
    for json in [false, true] {
        let output = fixture
            .preview("run", json)
            .env("OPENAI_API_KEY", "secret-openai")
            .env("ANTHROPIC_API_KEY", "secret-anthropic")
            .env("CLAUDE_CODE_OAUTH_TOKEN", "secret-oauth")
            .output()?;
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout)?;
        for name in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ] {
            assert!(text.contains(name));
        }
        for secret in ["secret-openai", "secret-anthropic", "secret-oauth"] {
            assert!(!text.contains(secret));
        }
        assert_eq!(output.stderr, b"");
    }
    Ok(())
}

#[test]
fn dry_run_redacts_custom_runtime_secrets() -> TestResult {
    dry_run_reports_credential_availability_without_values()
}

#[test]
fn spawn_dry_run_redacts_declared_secret_overrides() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.change_profile(|profile| {
        profile["security"]["runtime_secrets"]["CUSTOM_TOKEN"] = json!("required");
    })?;
    fs::write(
        &fixture.spawn,
        serde_json::to_vec(
            &json!({"workspace": fixture.workspace, "env": [["CUSTOM_TOKEN", "spawn-confidential"]], "agent_args": []}),
        )?,
    )?;
    for json in [false, true] {
        let output = fixture.preview("spawn", json).output()?;
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout)?;
        assert!(text.contains("CUSTOM_TOKEN"));
        assert!(text.contains("spawn_environment"));
        assert!(!text.contains("spawn-confidential"));
        assert_eq!(output.stderr, b"");
    }
    Ok(())
}
