use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use wrix_sandbox::command::{self, Command};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const PUBLIC_KEY: &str = "wrix-cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

#[test]
fn run_and_spawn_inject_container_pull_config() -> TestResult {
    let fixture = Fixture::new("pull-config")?;
    let run_output = fixture.render("run")?;
    let spawn_output = fixture.render("spawn")?;
    let cache_host = expected_sandbox_cache_host();
    let cache_url = format!("http://{cache_host}:21042");

    for output in [&run_output, &spawn_output] {
        assert!(output.contains(&format!("PROJECT_CACHE_URL={cache_url}")));
        assert!(output.contains(&format!("ENV=WRIX_PROJECT_CACHE_HOST={cache_host}")));
        assert!(output.contains("ENV=WRIX_PROJECT_CACHE_PORT=21042"));
        assert!(output.contains(&format!("extra-substituters = {cache_url}")));
        assert!(output.contains(&format!("extra-trusted-public-keys = {PUBLIC_KEY}")));
        assert!(output.contains("builders-use-substitutes = true"));
    }
    assert!(run_output.contains("SUBCOMMAND=run"));
    assert!(spawn_output.contains("SUBCOMMAND=spawn"));

    Ok(())
}

#[test]
fn sandbox_cache_integration_excludes_host_store_and_secrets() -> TestResult {
    let fixture = Fixture::new("excludes-secrets")?;
    let output = fixture.render("spawn")?;

    assert!(output.contains("PROJECT_CACHE_URL="));
    assert!(output.contains("ENV=NIX_CONFIG="));
    assert!(!output.contains("cache.secret"));
    assert!(!output.contains(&fixture.state_root.display().to_string()));
    assert!(!output.contains("/nix/var/nix/daemon-socket"));
    assert!(!output.contains("MOUNT=-v /nix/store"));
    assert!(!output.contains("host-store"));

    Ok(())
}

#[test]
#[ignore = "child process helper receives per-test environment"]
fn project_cache_child() -> TestResult {
    let case = env::var("WRIX_SANDBOX_TEST_CASE")?;
    let profile_config = PathBuf::from(env::var("WRIX_SANDBOX_PROFILE_CONFIG")?);
    let stdout_path = PathBuf::from(env::var("WRIX_SANDBOX_STDOUT")?);
    let stderr_path = PathBuf::from(env::var("WRIX_SANDBOX_STDERR")?);
    let (command, args) = match case.as_str() {
        "run" => {
            let workspace = env::var("WRIX_SANDBOX_WORKSPACE")?;
            (
                Command::Run,
                vec![workspace, String::from("echo"), String::from("hello")],
            )
        }
        "spawn" => {
            let spawn_config = env::var("WRIX_SANDBOX_SPAWN_CONFIG")?;
            (
                Command::Spawn,
                vec![
                    String::from("--spawn-config"),
                    spawn_config,
                    String::from("--stdio"),
                ],
            )
        }
        other => return Err(io::Error::other(format!("unknown child case {other}")).into()),
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = command::run(
        command,
        Some(profile_config),
        &args,
        &mut stdout,
        &mut stderr,
    )?;
    fs::write(&stdout_path, &stdout)?;
    fs::write(&stderr_path, &stderr)?;
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    Ok(())
}

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    profile_config: PathBuf,
    spawn_config: PathBuf,
    service_bin: PathBuf,
    endpoints: PathBuf,
    state_root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> TestResult<Self> {
        let root = tempfile::Builder::new().prefix(name).tempdir()?;
        let workspace = root.path().join("workspace");
        let state_root = root.path().join("state/workspace-hash");
        let cache_root = root.path().join("cache/workspace-hash/binary-cache");
        fs::create_dir_all(&workspace)?;
        fs::create_dir_all(state_root.join("keys"))?;
        fs::create_dir_all(&cache_root)?;
        fs::write(state_root.join("keys/cache.pub"), format!("{PUBLIC_KEY}\n"))?;
        let profile_config = root.path().join("profile.json");
        let spawn_config = root.path().join("spawn.json");
        let endpoints = root.path().join("endpoints.json");
        let service_bin = root.path().join("fake-wrix-service");
        write_profile_config(&profile_config)?;
        write_spawn_config(&spawn_config, &workspace)?;
        write_endpoints(&endpoints, &workspace, &state_root, &cache_root)?;
        write_fake_service(&service_bin)?;
        Ok(Self {
            root,
            workspace,
            profile_config,
            spawn_config,
            service_bin,
            endpoints,
            state_root,
        })
    }

    fn render(&self, case: &str) -> TestResult<String> {
        let stdout = self.root.path().join(format!("{case}.out"));
        let stderr = self.root.path().join(format!("{case}.err"));
        let output = ProcessCommand::new(env::current_exe()?)
            .arg("project_cache_child")
            .arg("--exact")
            .arg("--ignored")
            .env("WRIX_SANDBOX_TEST_CASE", case)
            .env("WRIX_SANDBOX_PROFILE_CONFIG", &self.profile_config)
            .env("WRIX_SANDBOX_SPAWN_CONFIG", &self.spawn_config)
            .env("WRIX_SANDBOX_WORKSPACE", &self.workspace)
            .env("WRIX_SANDBOX_STDOUT", &stdout)
            .env("WRIX_SANDBOX_STDERR", &stderr)
            .env("WRIX_DRY_RUN", "1")
            .env("WRIX_DRY_RUN_SERVICES", "1")
            .env("WRIX_DRY_RUN_SERVICE_BIN", &self.service_bin)
            .env("WRIX_FAKE_SERVICE_ENDPOINTS", &self.endpoints)
            .env("HOME", self.root.path().join("home"))
            .env("GIT_AUTHOR_NAME", "Wrix Test")
            .env("GIT_AUTHOR_EMAIL", "wrix@example.test")
            .env("GIT_COMMITTER_NAME", "Wrix Test")
            .env("GIT_COMMITTER_EMAIL", "wrix@example.test")
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "child failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }
        let child_stderr = fs::read_to_string(&stderr)?;
        assert!(child_stderr.is_empty(), "{child_stderr}");
        Ok(fs::read_to_string(stdout)?)
    }
}

fn write_profile_config(path: &PathBuf) -> io::Result<()> {
    fs::write(
        path,
        format!(
            r#"{{
  "schema": 1,
  "profile": {{
    "name": "base",
    "env": {{}},
    "mounts": [],
    "writable_dirs": [],
    "network_allowlist": []
  }},
  "image": {{
    "ref": "localhost/wrix-test:latest",
    "source": "/image-source",
    "source_kind": "{}",
    "digest": "sha256:{}"
  }},
  "agent": {{ "kind": "direct" }},
  "services": {{ "nix_cache": {{ "enable": true }} }}
}}
"#,
            expected_source_kind(),
            "a".repeat(64)
        ),
    )
}

fn write_spawn_config(path: &PathBuf, workspace: &Path) -> io::Result<()> {
    fs::write(
        path,
        format!(
            r#"{{
  "workspace": "{}",
  "env": [],
  "agent_args": ["echo", "hello"],
  "mounts": []
}}
"#,
            json_path(workspace)
        ),
    )
}

fn write_endpoints(
    path: &PathBuf,
    workspace: &Path,
    state_root: &Path,
    cache_root: &Path,
) -> io::Result<()> {
    fs::write(
        path,
        format!(
            r#"{{
  "schema_version": 1,
  "workspace_path": "{}",
  "workspace_hash": "{}",
  "container_name": "workspace-service",
  "state_root": "{}",
  "cache_root": "{}",
  "endpoints": {{
    "cache_http": {{ "host": "127.0.0.1", "port": 21042 }},
    "dolt": null,
    "dolt_unix": null,
    "dolt_tcp": null
  }}
}}
"#,
            json_path(workspace),
            "a".repeat(64),
            json_path(state_root),
            json_path(cache_root)
        ),
    )
}

fn write_fake_service(path: &PathBuf) -> io::Result<()> {
    fs::write(
        path,
        r#"#!/usr/bin/env bash
set -euo pipefail

case "$*" in
  "service start")
    exit 0
    ;;
  "service endpoints")
    cat "$WRIX_FAKE_SERVICE_ENDPOINTS"
    ;;
  *)
    printf 'unsupported fake service command: %s\n' "$*" >&2
    exit 2
    ;;
esac
"#,
    )?;
    make_executable(path)
}

const fn expected_source_kind() -> &'static str {
    if cfg!(target_os = "macos") {
        "docker-archive"
    } else {
        "nix-descriptor"
    }
}

const fn expected_sandbox_cache_host() -> &'static str {
    if cfg!(target_os = "macos") {
        "127.0.0.1"
    } else {
        "169.254.1.2"
    }
}

fn json_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}
