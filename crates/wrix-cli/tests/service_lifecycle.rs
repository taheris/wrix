mod common;

use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use common::{RunResult, TestResult, run_command, set_mode, wrix_command_with_path};
use serde_json::{Value, json};
use wrix_sandbox::command::Command as LaunchCommand;

const APPLE_RUNTIME: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${WRIX_TEST_RUNTIME_LOG:?}"
case "$*" in
  'list --all --format json') ;;
  "inspect ${WRIX_TEST_SERVICE_NAME:?}") ;;
  "rm -f ${WRIX_TEST_SERVICE_NAME:?}")
    printf '[]\n' >"${WRIX_TEST_RUNTIME_JSON:?}"
    exit 0
    ;;
  "run -d --name ${WRIX_TEST_SERVICE_NAME:?} "*)
    printf '%s\n' "$@" >"${WRIX_TEST_RUN_ARGS:?}"
    cp "${WRIX_TEST_RUNTIME_REPLACEMENT:?}" "${WRIX_TEST_RUNTIME_JSON:?}"
    exit 0
    ;;
  *) printf 'unexpected Apple container command: %s\n' "$*" >&2; exit 64 ;;
esac
cat "${WRIX_TEST_RUNTIME_JSON:?}"
"#;

#[test]
fn malformed_sync_branch_fails_before_service_runtime_or_layout() -> TestResult {
    let fixture = Fixture::new()?;
    let state_root = fixture.state_root()?;
    let before = fs::read(state_root.join("services.json"))?;
    fs::write(
        fixture.workspace.join(".beads/config.yaml"),
        "sync-branch: [\n",
    )?;
    let output = fixture.run("start")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("invalid beads configuration"),
        "{}",
        output.stderr
    );
    assert!(!fixture.root.path().join("runtime.log").exists());
    assert!(!state_root.join("service.lock").exists());
    assert!(!state_root.join("gcroots").exists());
    assert_eq!(fs::read(state_root.join("services.json"))?, before);
    Ok(())
}

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    runtime: PathBuf,
    metadata: Value,
    snapshot: Value,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        // Repository-local TMPDIR paths inherit the enclosing checkout's service identity.
        let root = tempfile::Builder::new()
            .prefix("apple-service")
            .tempdir_in("/tmp")?;
        Self::from_root(root)
    }

    fn from_root(root: tempfile::TempDir) -> TestResult<Self> {
        let workspace = root.path().join("workspace");
        let runtime = root.path().join("container");
        fs::create_dir_all(workspace.join(".beads/dolt"))?;
        initialize_repository(&workspace)?;
        fs::write(&runtime, APPLE_RUNTIME)?;
        set_mode(&runtime, 0o755)?;
        let mut fixture = Self {
            root,
            workspace,
            runtime,
            metadata: Value::Null,
            snapshot: Value::Null,
        };
        let output = fixture.run("endpoints")?;
        assert!(output.status.success(), "{}", output.stderr);
        fixture.metadata = serde_json::from_str(&output.stdout)?;
        assert_eq!(
            fixture.metadata["workspace_path"].as_str(),
            fixture.workspace.canonicalize()?.to_str(),
            "fixture resolved another workspace; CLI: {}; endpoints: {}",
            env!("CARGO_BIN_EXE_wrix"),
            fixture.metadata,
        );
        fixture.snapshot = json!([{
            "configuration": {
                "id": fixture.metadata["container_name"],
                "labels": {
                    "wrix.kind": "service",
                    "wrix.workspace": fixture.metadata["workspace_path"],
                    "wrix.workspace.hash": fixture.metadata["workspace_hash"],
                    "wrix.cache.enabled": "true",
                    "wrix.service.supervision": "1",
                    "wrix.dolt.transport": "tcp",
                    "wrix.dolt.auth": "tcp-root-v1"
                },
                "publishedPorts": [
                    {"hostAddress": "127.0.0.1", "hostPort": fixture.port("cache_http")?, "containerPort": 8080, "proto": "tcp", "count": 1},
                    {"hostAddress": "127.0.0.1", "hostPort": fixture.port("dolt_tcp")?, "containerPort": 3306, "proto": "tcp", "count": 1}
                ]
            },
            "status": {
                "state": "running",
                "networks": [{"ipv4Address": "192.168.64.12/24"}]
            }
        }]);
        fixture.write_snapshot()?;
        fs::write(
            fixture.root.path().join("replacement.json"),
            serde_json::to_vec_pretty(&fixture.snapshot)?,
        )?;
        let state_root = fixture.state_root()?;
        fs::create_dir_all(state_root.join("keys"))?;
        fs::write(
            state_root.join("services.json"),
            serde_json::to_vec_pretty(&fixture.metadata)?,
        )?;
        fs::write(state_root.join("keys/cache.secret"), "fixture-secret\n")?;
        fs::write(
            state_root.join("keys/cache.pub"),
            "wrix-cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
        )?;
        Ok(fixture)
    }

    fn command(&self) -> TestResult<Command> {
        let mut command =
            wrix_command_with_path(&self.workspace, &[&self.root.path().join("bin")])?;
        command
            .env("HOME", self.root.path().join("home"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("WRIX_CONTAINER_RUNTIME", &self.runtime)
            .env("WRIX_DOLT_TRANSPORT", "tcp")
            .env("WRIX_SERVICE_ALLOW_TEMP_CACHE", "1")
            .env(
                "WRIX_TEST_RUNTIME_LOG",
                self.root.path().join("runtime.log"),
            )
            .env(
                "WRIX_TEST_RUNTIME_JSON",
                self.root.path().join("runtime.json"),
            )
            .env("WRIX_TEST_RUN_ARGS", self.root.path().join("run.argv"))
            .env(
                "WRIX_TEST_RUNTIME_REPLACEMENT",
                self.root.path().join("replacement.json"),
            )
            .env(
                "WRIX_TEST_SERVICE_NAME",
                self.metadata["container_name"].as_str().unwrap_or("unused"),
            )
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE")
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE_KIND")
            .env_remove("WRIX_SERVICE_IMAGE_DIGEST");
        Ok(command)
    }

    fn runtime_command(&self) -> TestResult<Command> {
        let environment = self.command()?;
        let mut command = Command::new(&self.runtime);
        command.envs(
            environment
                .get_envs()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        );
        Ok(command)
    }

    fn run(&self, operation: &str) -> TestResult<RunResult> {
        run_command(self.command()?.args(["service", operation]))
    }

    fn port(&self, endpoint: &str) -> TestResult<u16> {
        serde_json::from_value(self.metadata["endpoints"][endpoint]["port"].clone())
            .map_err(|error| {
                format!(
                    "invalid fixture endpoint {endpoint} port: {error}; CLI: {}; workspace: {}; endpoints: {}",
                    env!("CARGO_BIN_EXE_wrix"),
                    self.workspace.display(),
                    self.metadata,
                )
                .into()
            })
    }

    fn state_root(&self) -> TestResult<PathBuf> {
        Ok(serde_json::from_value(self.metadata["state_root"].clone())?)
    }

    fn disable_dolt(&mut self) -> TestResult {
        fs::remove_dir(self.workspace.join(".beads/dolt"))?;
        self.snapshot[0]["configuration"]["labels"]["wrix.dolt.transport"] = json!("disabled");
        self.write_snapshot()
    }

    fn launch_command(&self, mode: LaunchCommand, workspace: &Path) -> TestResult<Command> {
        let bin = self.root.path().join("bin");
        fs::create_dir_all(&bin)?;
        for name in ["podman", "container", "route"] {
            let path = bin.join(name);
            fs::write(&path, include_str!("fixtures/launch-runtime.sh"))?;
            set_mode(&path, 0o755)?;
        }
        let profile = self.root.path().join("profile.json");
        let digest = format!("sha256:{}", "a".repeat(64));
        fs::write(
            &profile,
            serde_json::to_vec(&json!({
                "schema": 1,
                "system": "test",
                "profile": {"name": "base"},
                "image": {
                    "ref": "localhost/wrix-test:latest",
                    "source": "/missing/image-source",
                    "source_kind": if cfg!(target_os = "macos") {"docker-archive"} else {"nix-descriptor"},
                    "digest": digest
                },
                "agent": {"kind": "direct"},
                "services": {"nix_cache": {"enable": true}}
            }))?,
        )?;
        let mut command = self.command()?;
        command
            .env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("WRIX_IMAGE_KEEP_FILE", self.root.path().join("mru.json"))
            .env("WRIX_TEST_DIGEST", digest)
            .env("WRIX_TEST_ARGV", self.root.path().join("argv"))
            .env("WRIX_NETWORK", "limit")
            .arg("--profile-config")
            .arg(profile)
            .arg(match mode {
                LaunchCommand::Run => "run",
                LaunchCommand::Spawn => "spawn",
            });
        for name in [
            "WRIX_DRY_RUN",
            "WRIX_DRY_RUN_SERVICES",
            "WRIX_PROJECT_CACHE_SANDBOX_HOST",
            "WRIX_MICROVM",
            "WRIX_UNSAFE_PODMAN_SOCKET",
            "WRIX_FOCUS_TARGET",
            "TMUX",
        ] {
            command.env_remove(name);
        }
        if mode == LaunchCommand::Run {
            command.arg(workspace).arg("guest-marker");
        } else {
            let spawn = self.root.path().join("spawn.json");
            fs::write(
                &spawn,
                serde_json::to_vec(
                    &json!({"workspace": workspace, "env": [], "agent_args": ["guest-marker"]}),
                )?,
            )?;
            command.arg("--spawn-config").arg(spawn).arg("--stdio");
        }
        Ok(command)
    }

    fn write_snapshot(&self) -> TestResult {
        fs::write(
            self.root.path().join("runtime.json"),
            serde_json::to_vec_pretty(&self.snapshot)?,
        )?;
        Ok(())
    }
}

fn initialize_repository(path: &Path) -> TestResult {
    let output = run_command(
        Command::new(common::command_path("git")?)
            .env_clear()
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(path)
            .args(["init", "-q"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    Ok(())
}

#[test]
fn enclosing_repository_does_not_change_service_fixture_identity() -> TestResult {
    let outer = tempfile::Builder::new()
        .prefix("service-fixture-enclosing-repository")
        .tempdir_in("/tmp")?;
    initialize_repository(outer.path())?;
    let root = tempfile::Builder::new()
        .prefix("apple-service")
        .tempdir_in(outer.path())?;
    let fixture = Fixture::from_root(root)?;
    assert_eq!(
        fixture.metadata["workspace_path"].as_str(),
        fixture.workspace.canonicalize()?.to_str(),
    );
    assert!(fixture.workspace.join(".git").is_dir());
    assert_ne!(fixture.port("cache_http")?, 0);
    assert_ne!(fixture.port("dolt_tcp")?, 0);
    assert!(!outer.path().join(".wrix").exists());
    Ok(())
}

#[test]
fn repository_local_tempdir_does_not_change_service_fixture_identity() -> TestResult {
    let outer = tempfile::Builder::new()
        .prefix("service-fixture-repository")
        .tempdir_in("/tmp")?;
    initialize_repository(outer.path())?;
    let tmpdir = outer.path().join(".loom/scratch");
    fs::create_dir_all(&tmpdir)?;
    let output = run_command(
        Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "apple_sandbox_endpoint_uses_service_vm_instead_of_host_loopback",
                "--nocapture",
            ])
            .env("TMPDIR", tmpdir)
            .env("GIT_DIR", outer.path().join(".git"))
            .env("GIT_COMMON_DIR", outer.path().join(".git"))
            .env("GIT_WORK_TREE", outer.path()),
    )?;
    assert!(
        output.status.success(),
        "{} {}",
        output.stdout,
        output.stderr
    );
    assert!(!outer.path().join(".wrix").exists());
    Ok(())
}

#[test]
fn null_fixture_endpoint_reports_cli_workspace_and_payload() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.metadata["endpoints"]["dolt_tcp"] = Value::Null;
    let error = fixture.port("dolt_tcp").unwrap_err().to_string();
    assert!(error.contains("dolt_tcp"), "{error}");
    assert!(error.contains(env!("CARGO_BIN_EXE_wrix")), "{error}");
    assert!(
        error.contains(&fixture.workspace.display().to_string()),
        "{error}"
    );
    assert!(error.contains(&fixture.metadata.to_string()), "{error}");
    Ok(())
}

#[test]
fn repeated_apple_service_start_preserves_busy_owned_endpoints() -> TestResult {
    let fixture = Fixture::new()?;
    let _cache = TcpListener::bind(("127.0.0.1", fixture.port("cache_http")?))?;
    let _dolt = TcpListener::bind(("127.0.0.1", fixture.port("dolt_tcp")?))?;

    for _ in 0..2 {
        let output = fixture.run("start")?;
        assert!(output.status.success(), "{}", output.stderr);
        assert!(
            output.stdout.contains("runtime: Running"),
            "{}",
            output.stdout
        );
        let persisted: Value =
            serde_json::from_slice(&fs::read(fixture.state_root()?.join("services.json"))?)?;
        assert_eq!(persisted["endpoints"], fixture.metadata["endpoints"]);
        let exported: Value = serde_json::from_str(&fixture.run("endpoints")?.stdout)?;
        assert_eq!(exported["endpoints"], fixture.metadata["endpoints"]);
    }
    let log = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert!(
        log.lines()
            .all(|line| line.starts_with("list ") || line.starts_with("inspect ")),
        "{log}"
    );
    Ok(())
}

#[test]
fn apple_start_recreates_legacy_tcp_services_for_authentication_bootstrap() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.snapshot[0]["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.dolt.auth");
    fixture.write_snapshot()?;
    let marker = fixture.workspace.join(".beads/dolt/keep");
    fs::write(&marker, "persistent database")?;

    for _ in 0..2 {
        let output = fixture.run("start")?;
        assert!(output.status.success(), "{}", output.stderr);
    }
    let log = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("rm -f "))
            .count(),
        1,
        "{log}"
    );
    assert_eq!(
        log.lines().filter(|line| line.starts_with("run ")).count(),
        1,
        "{log}"
    );
    let args = fs::read_to_string(fixture.root.path().join("run.argv"))?;
    assert!(
        args.lines().any(|arg| arg == "wrix.dolt.auth=tcp-root-v1"),
        "{args}"
    );
    assert!(
        args.lines()
            .any(|arg| arg == format!("127.0.0.1:{}:3306", fixture.port("dolt_tcp").unwrap())),
        "{args}"
    );
    assert!(
        args.contains("CREATE USER IF NOT EXISTS 'root'@'%'"),
        "{args}"
    );
    assert_eq!(fs::read_to_string(marker)?, "persistent database");
    Ok(())
}

#[test]
fn legacy_combined_services_are_recreated_with_child_supervision() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.snapshot[0]["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.service.supervision");
    fixture.write_snapshot()?;
    for _ in 0..2 {
        let output = fixture.run("start")?;
        assert!(output.status.success(), "{}", output.stderr);
    }
    let log = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert_eq!(
        log.lines().filter(|line| line.starts_with("run ")).count(),
        1,
        "{log}"
    );
    let args = fs::read_to_string(fixture.root.path().join("run.argv"))?;
    assert!(args.contains("wrix.service.supervision=1"));
    assert!(args.contains("wait -n"));
    Ok(())
}

#[test]
fn tcp_wait_rejects_a_listener_without_sql_authentication() -> TestResult {
    let fixture = Fixture::new()?;
    let _listener = TcpListener::bind(("127.0.0.1", fixture.port("dolt_tcp")?))?;
    let start = Instant::now();
    let output = run_command(fixture.command()?.args(["service", "dolt", "wait"]))?;
    assert!(start.elapsed() < Duration::from_secs(8));
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("did not become ready"),
        "{}",
        output.stderr
    );
    assert!(
        output.stderr.contains("SQL authentication probe"),
        "{}",
        output.stderr
    );
    Ok(())
}

#[test]
fn apple_sandbox_endpoint_uses_service_vm_instead_of_host_loopback() -> TestResult {
    let mut fixture = Fixture::new()?;
    let output = run_command(
        fixture
            .command()?
            .args(["service", "dolt", "sandbox-endpoint"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let endpoint: Value = serde_json::from_str(&output.stdout)?;
    assert_eq!(endpoint, json!({"host": "192.168.64.12", "port": 3306}));
    fixture.snapshot[0]["status"]["networks"] = json!([]);
    fixture.write_snapshot()?;
    let output = run_command(
        fixture
            .command()?
            .args(["service", "dolt", "sandbox-endpoint"]),
    )?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("no sandbox-reachable"),
        "{}",
        output.stderr
    );
    Ok(())
}

#[test]
fn apple_sandbox_endpoint_accepts_legacy_network_layout() -> TestResult {
    let mut fixture = Fixture::new()?;
    let networks = fixture.snapshot[0]["status"]["networks"].take();
    fixture.snapshot[0]["status"] = json!("running");
    fixture.snapshot[0]["networks"] = networks;
    fixture.write_snapshot()?;
    let output = run_command(
        fixture
            .command()?
            .args(["service", "dolt", "sandbox-endpoint"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let endpoint: Value = serde_json::from_str(&output.stdout)?;
    assert_eq!(endpoint, json!({"host": "192.168.64.12", "port": 3306}));
    Ok(())
}

#[test]
fn apple_cache_endpoint_uses_service_vm_without_dolt_or_host_metadata_changes() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    let persisted = fixture.state_root()?.join("services.json");
    let before = fs::read(&persisted)?;
    let output = run_command(
        fixture
            .command()?
            .args(["service", "endpoints", "--sandbox-cache"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let metadata: Value = serde_json::from_str(&output.stdout)?;
    assert_eq!(
        metadata["endpoints"]["cache_http"],
        json!({"host": "192.168.64.12", "port": 8080})
    );
    assert_eq!(metadata["endpoints"]["dolt"], Value::Null);
    assert_eq!(fs::read(persisted)?, before);
    let host: Value = serde_json::from_str(&fixture.run("endpoints")?.stdout)?;
    assert_eq!(
        host["endpoints"]["cache_http"],
        fixture.metadata["endpoints"]["cache_http"]
    );
    assert_eq!(host["endpoints"]["cache_http"]["host"], "127.0.0.1");
    assert!((21000..=22999).contains(&fixture.port("cache_http")?));
    Ok(())
}

#[test]
fn apple_cache_endpoint_accepts_legacy_network_layout() -> TestResult {
    let mut fixture = Fixture::new()?;
    let networks = fixture.snapshot[0]["status"]["networks"].take();
    fixture.snapshot[0]["status"] = json!("running");
    fixture.snapshot[0]["networks"] = networks;
    fixture.write_snapshot()?;
    let output = run_command(
        fixture
            .command()?
            .args(["service", "endpoints", "--sandbox-cache"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let metadata: Value = serde_json::from_str(&output.stdout)?;
    assert_eq!(
        metadata["endpoints"]["cache_http"],
        json!({"host": "192.168.64.12", "port": 8080})
    );
    Ok(())
}

#[test]
fn apple_cache_endpoint_skips_unusable_interfaces() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.snapshot[0]["status"]["networks"] = json!([
        {"ipv4Address": "127.0.0.1/8"},
        {"ipv4Address": "0.0.0.0/0"},
        {"ipv4Address": "invalid"},
        {"ipv4Address": "192.168.64.22/24"}
    ]);
    fixture.write_snapshot()?;
    let output = run_command(
        fixture
            .command()?
            .args(["service", "endpoints", "--sandbox-cache"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let metadata: Value = serde_json::from_str(&output.stdout)?;
    assert_eq!(
        metadata["endpoints"]["cache_http"],
        json!({"host": "192.168.64.22", "port": 8080})
    );
    Ok(())
}

#[test]
fn apple_cache_endpoint_rejects_missing_or_stopped_guest_addresses() -> TestResult {
    let mut fixture = Fixture::new()?;
    let running = fixture.snapshot.clone();
    for status in [
        json!({"state": "running", "networks": []}),
        json!({"state": "running", "networks": [{"ipv4Address": "127.0.0.1/8"}]}),
        json!({"state": "running", "networks": [{"ipv4Address": "0.0.0.0/0"}]}),
        json!({"state": "running", "networks": [{"ipv4Address": "invalid"}]}),
        json!({"state": "stopped", "networks": [{"ipv4Address": "192.168.64.12/24"}]}),
        Value::Null,
    ] {
        fixture.snapshot = running.clone();
        fixture.snapshot[0]["networks"] = json!([{"ipv4Address": "192.168.64.99/24"}]);
        if status.is_null() {
            fixture.snapshot = json!([]);
        } else {
            fixture.snapshot[0]["status"] = status;
        }
        fixture.write_snapshot()?;
        let output =
            run_command(
                fixture
                    .command()?
                    .args(["service", "endpoints", "--sandbox-cache"]),
            )?;
        assert!(!output.status.success(), "accepted {}", fixture.snapshot);
        assert!(
            output.stderr.contains("no sandbox-reachable project cache"),
            "{}",
            output.stderr
        );
        assert_eq!(output.stdout, "");
    }
    Ok(())
}

#[test]
fn disabled_cache_endpoint_does_not_inspect_the_runtime() -> TestResult {
    let fixture = Fixture::new()?;
    let output = run_command(fixture.command()?.args([
        "service",
        "endpoints",
        "--sandbox-cache",
        "--no-cache",
    ]))?;
    assert!(output.status.success(), "{}", output.stderr);
    let metadata: Value = serde_json::from_str(&output.stdout)?;
    assert_eq!(metadata["endpoints"]["cache_http"], Value::Null);
    assert!(!fixture.root.path().join("runtime.log").exists());
    Ok(())
}

#[test]
fn sandbox_cache_endpoint_respects_the_selected_runtime() -> TestResult {
    let fixture = Fixture::new()?;
    let output = run_command(
        fixture
            .command()?
            .env("WRIX_CONTAINER_RUNTIME", fixture.root.path().join("podman"))
            .args(["service", "endpoints", "--sandbox-cache"]),
    )?;
    if cfg!(target_os = "linux") {
        assert!(output.status.success(), "{}", output.stderr);
        let metadata: Value = serde_json::from_str(&output.stdout)?;
        assert_eq!(
            metadata["endpoints"]["cache_http"],
            json!({"host": "169.254.1.2", "port": fixture.port("cache_http")?})
        );
    } else {
        assert!(!output.status.success());
        assert!(
            output.stderr.contains("no sandbox-reachable project cache"),
            "{}",
            output.stderr
        );
    }
    assert!(!fixture.root.path().join("runtime.log").exists());
    Ok(())
}

#[test]
fn cache_guest_endpoint_and_key_reach_both_launch_commands() -> TestResult {
    for mode in [LaunchCommand::Run, LaunchCommand::Spawn] {
        for layout in ["", ".loom/beads/cache-route", ".loom/integration"] {
            let mut fixture = Fixture::new()?;
            fixture.disable_dolt()?;
            let workspace = fixture.workspace.join(layout);
            fs::create_dir_all(&workspace)?;
            if !layout.is_empty() {
                initialize_repository(&workspace)?;
            }
            let output = run_command(&mut fixture.launch_command(mode, &workspace)?)?;
            assert!(
                output.status.success(),
                "{mode:?}/{layout}: {}",
                output.stderr
            );
            let bytes = fs::read(fixture.root.path().join("argv"))?;
            let argv: Vec<_> = bytes
                .strip_suffix(&[0])
                .ok_or("argv capture lacks trailing NUL")?
                .split(|byte| *byte == 0)
                .map(|arg| std::str::from_utf8(arg))
                .collect::<Result<_, _>>()?;
            let env: Vec<_> = argv
                .windows(2)
                .filter(|pair| pair[0] == "-e")
                .map(|pair| pair[1])
                .collect();
            assert_eq!(
                env.iter()
                    .filter(|value| value.starts_with("WRIX_PROJECT_CACHE_"))
                    .copied()
                    .collect::<Vec<_>>(),
                [
                    "WRIX_PROJECT_CACHE_HOST=192.168.64.12",
                    "WRIX_PROJECT_CACHE_PORT=8080"
                ]
            );
            let public_key = fs::read_to_string(fixture.state_root()?.join("keys/cache.pub"))?;
            assert_eq!(
                env.iter()
                    .filter(|value| value.starts_with("NIX_CONFIG="))
                    .copied()
                    .collect::<Vec<_>>(),
                [format!(
                    "NIX_CONFIG=extra-substituters = http://192.168.64.12:8080\nextra-trusted-public-keys = {}\nbuilders-use-substitutes = true",
                    public_key.trim()
                )]
            );
            assert!(env.contains(&"WRIX_NETWORK=limit"));
            assert!(
                !env.iter()
                    .any(|value| value.starts_with("BEADS_DOLT_SERVER_")
                        || value.starts_with("WRIX_NETWORK_LOCAL_ENDPOINTS="))
            );
            assert_eq!(argv.last().copied(), Some("guest-marker"));
            let state_root = fixture.state_root()?;
            let cache_root = fixture.metadata["cache_root"]
                .as_str()
                .ok_or("cache root missing")?;
            assert!(
                !argv
                    .iter()
                    .any(|arg| arg.contains(state_root.to_str().unwrap())
                        || arg.contains(cache_root)
                        || arg.contains("cache.secret")
                        || arg.contains(":/nix/store")
                        || arg.contains("daemon-socket"))
            );
            let persisted: Value =
                serde_json::from_slice(&fs::read(state_root.join("services.json"))?)?;
            assert_eq!(
                persisted["endpoints"]["cache_http"],
                fixture.metadata["endpoints"]["cache_http"]
            );
            let log = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
            assert!(log.lines().any(|line| line
                == format!(
                    "inspect {}",
                    fixture.metadata["container_name"].as_str().unwrap()
                )));
            assert!(log.lines().all(|line| !line.starts_with("run ")), "{log}");
        }
    }
    Ok(())
}

#[test]
fn cache_endpoint_generates_only_an_exact_tcp_network_exception() -> TestResult {
    let bootstrap = include_str!("../../../lib/sandbox/network-bootstrap.sh");
    let policy = bootstrap
        .split_once("# BEGIN wrix network policy\n")
        .ok_or("network policy missing")?
        .1
        .split_once("# END wrix network policy")
        .ok_or("network policy end missing")?
        .0;
    for mode in [LaunchCommand::Run, LaunchCommand::Spawn] {
        let mut fixture = Fixture::new()?;
        fixture.disable_dolt()?;
        let output = run_command(&mut fixture.launch_command(mode, &fixture.workspace)?)?;
        assert!(output.status.success(), "{}", output.stderr);
        let bytes = fs::read(fixture.root.path().join("argv"))?;
        let argv: Vec<_> = bytes
            .strip_suffix(&[0])
            .ok_or("argv capture lacks trailing NUL")?
            .split(|byte| *byte == 0)
            .map(std::str::from_utf8)
            .collect::<Result<_, _>>()?;
        let cache_env: Vec<_> = argv
            .windows(2)
            .filter(|pair| pair[0] == "-e")
            .filter_map(|pair| pair[1].split_once('='))
            .filter(|(name, _)| name.starts_with("WRIX_PROJECT_CACHE_"))
            .collect();
        for (backend, expected) in [
            (
                "nft",
                "add rule inet wrix output ip daddr 192.168.64.12 tcp dport 8080 accept\n",
            ),
            (
                "iptables",
                "-w -A OUTPUT -p tcp -d 192.168.64.12 --dport 8080 -j ACCEPT\n",
            ),
        ] {
            let output = Command::new(common::command_path("bash")?)
                .env_clear()
                .envs(cache_env.iter().copied())
                .env("WRIX_FIREWALL_BACKEND", backend)
                .env("WRIX_NFT_BIN", common::command_path("echo")?)
                .env("WRIX_IPTABLES_BIN", common::command_path("echo")?)
                .args([
                    "-c",
                    &format!("set -euo pipefail\n{policy}\nwrix_allow_local_endpoints"),
                ])
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(String::from_utf8(output.stdout)?, expected);
        }
    }
    Ok(())
}

#[test]
fn apple_cache_service_preserves_loopback_publication_and_read_only_mount() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fixture.snapshot[0]["configuration"]["labels"]["wrix.cache.enabled"] = json!("false");
    fixture.write_snapshot()?;
    let output = fixture.run("start")?;
    assert!(output.status.success(), "{}", output.stderr);
    let args = fs::read_to_string(fixture.root.path().join("run.argv"))?;
    let publications: Vec<_> = args
        .lines()
        .collect::<Vec<_>>()
        .windows(2)
        .filter(|pair| pair[0] == "-p")
        .map(|pair| pair[1])
        .collect();
    assert_eq!(
        publications,
        [format!("127.0.0.1:{}:8080", fixture.port("cache_http")?)]
    );
    assert!(args.lines().any(|arg| arg
        == format!(
            "{}:/cache:ro",
            fixture.metadata["cache_root"].as_str().unwrap()
        )));
    assert!(!args.contains("/var/lib/wrix/beads/dolt"));
    Ok(())
}

#[test]
fn launcher_fails_before_agent_start_without_a_guest_cache_address() -> TestResult {
    for mode in [LaunchCommand::Run, LaunchCommand::Spawn] {
        let mut fixture = Fixture::new()?;
        fixture.disable_dolt()?;
        fixture.snapshot[0]["status"]["networks"] = json!([]);
        fixture.write_snapshot()?;
        let output = run_command(&mut fixture.launch_command(mode, &fixture.workspace)?)?;
        assert!(!output.status.success());
        assert!(
            output.stderr.contains("no sandbox-reachable project cache"),
            "{}",
            output.stderr
        );
        assert!(!fixture.root.path().join("argv").exists());
    }
    Ok(())
}

#[test]
fn concurrent_service_starts_share_one_lifecycle_owner() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.snapshot[0]["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.dolt.auth");
    fixture.write_snapshot()?;
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            fixture
                .command()?
                .args(["service", "start"])
                .stdout(std::process::Stdio::null())
                .spawn()?,
        );
    }
    for mut child in children {
        assert!(child.wait()?.success());
    }
    let log = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert_eq!(
        log.lines().filter(|line| line.starts_with("run ")).count(),
        1,
        "{log}"
    );
    Ok(())
}

#[test]
fn managed_checkout_outside_devshell_skips_import_and_preserves_chained_hooks() -> TestResult {
    let fixture = Fixture::new()?;
    let root = &fixture.workspace;
    common::run_git(root, &["init", "-q"])?;
    common::run_git(root, &["config", "user.name", "Wrix Test"])?;
    common::run_git(root, &["config", "user.email", "test@example.invalid"])?;
    fs::write(
        root.join(".beads/config.yaml"),
        "sync:\n  mode: dolt-native\nsync-branch: beads\nimport.auto: true\nexport.auto: false\n",
    )?;
    fs::write(
        root.join(".beads/metadata.json"),
        "{\"backend\":\"dolt\",\"dolt_mode\":\"server\",\"dolt_database\":\"beads\",\"last_bd_version\":\"1.3.0\"}\n",
    )?;
    fs::write(root.join(".beads/.local_version"), "1.3.0\n")?;
    fs::write(
        root.join(".beads/issues.jsonl"),
        "legacy JSONL must not be imported\n",
    )?;
    common::run_git(root, &["add", ".beads/config.yaml", ".beads/metadata.json"])?;
    common::run_git(root, &["commit", "-qm", "fixture"])?;
    let output = fixture.run("start")?;
    assert!(output.status.success(), "{}", output.stderr);
    let hooks = root.join(".git/hooks");
    fs::write(
        hooks.join("post-checkout"),
        "#!/usr/bin/env bash\nset -euo pipefail\nexec bd hooks run post-checkout \"$@\"\n",
    )?;
    fs::write(
        hooks.join("post-checkout.old"),
        "#!/usr/bin/env bash\nset -euo pipefail\ntouch chained-hook-ran\n",
    )?;
    set_mode(&hooks.join("post-checkout"), 0o755)?;
    set_mode(&hooks.join("post-checkout.old"), 0o755)?;
    let mut command = common::git_command(root, &["checkout", "-b", "after-managed-start"]);
    command.env("HOME", fixture.root.path().join("home"));
    for (name, _) in std::env::vars() {
        if name.starts_with("BEADS_") || name.starts_with("BD_") {
            command.env_remove(name);
        }
    }
    let output = run_command(&mut command)?;
    assert!(output.status.success(), "{}", output.stderr);
    assert!(!output.stderr.contains("JSONL import"), "{}", output.stderr);
    assert!(
        !output.stderr.contains("no Dolt remote"),
        "{}",
        output.stderr
    );
    assert!(root.join("chained-hook-ran").is_file());
    assert!(!root.join(".beads/dolt-server.pid").exists());
    assert_eq!(
        fs::read_to_string(root.join(".beads/issues.jsonl"))?,
        "legacy JSONL must not be imported\n"
    );
    Ok(())
}

#[test]
fn apple_status_accepts_pretty_nested_and_legacy_string_states() -> TestResult {
    let mut fixture = Fixture::new()?;
    for state in [json!({"state": "running"}), json!("running")] {
        fixture.snapshot[0]["status"] = state;
        fixture.write_snapshot()?;
        let output = fixture.run("status")?;
        assert!(output.status.success(), "{}", output.stderr);
        assert!(
            output.stdout.contains("runtime: Running"),
            "{}",
            output.stdout
        );
    }
    Ok(())
}

#[test]
fn apple_empty_inspect_is_missing_not_stopped() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.snapshot = json!([]);
    fixture.write_snapshot()?;
    let output = fixture.run("status")?;
    assert!(output.status.success(), "{}", output.stderr);
    assert!(
        output.stdout.contains("runtime: Missing"),
        "{}",
        output.stdout
    );
    Ok(())
}

#[test]
fn malformed_apple_inventory_fails_without_rewriting_endpoints() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.path().join("runtime.json"), "not json\n")?;
    let before = fs::read(fixture.state_root()?.join("services.json"))?;
    let output = fixture.run("start")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("Apple container JSON"),
        "{}",
        output.stderr
    );
    assert_eq!(
        fs::read(fixture.state_root()?.join("services.json"))?,
        before
    );
    Ok(())
}

#[test]
fn fake_apple_runtime_exposes_native_json_and_rejects_podman_flags() -> TestResult {
    let fixture = Fixture::new()?;
    for args in [
        vec!["list", "--all", "--format", "json"],
        vec![
            "inspect",
            fixture.metadata["container_name"].as_str().unwrap(),
        ],
    ] {
        let output = Command::new(&fixture.runtime)
            .args(args)
            .env(
                "WRIX_TEST_RUNTIME_LOG",
                fixture.root.path().join("runtime.log"),
            )
            .env(
                "WRIX_TEST_RUNTIME_JSON",
                fixture.root.path().join("runtime.json"),
            )
            .env(
                "WRIX_TEST_SERVICE_NAME",
                fixture.metadata["container_name"].as_str().unwrap(),
            )
            .output()?;
        assert!(output.status.success());
        let actual: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(actual, fixture.snapshot);
        assert_eq!(actual[0]["status"]["state"], "running");
        assert_eq!(
            actual[0]["status"]["networks"][0]["ipv4Address"],
            "192.168.64.12/24"
        );
        assert!(actual[0].get("networks").is_none());
        assert_eq!(
            actual[0]["configuration"]["publishedPorts"][1]["hostPort"],
            fixture.port("dolt_tcp")?
        );
    }
    let output = Command::new(&fixture.runtime)
        .args([
            "inspect",
            "--format",
            "{{.State.Running}}",
            "workspace-service",
        ])
        .env(
            "WRIX_TEST_RUNTIME_LOG",
            fixture.root.path().join("runtime.log"),
        )
        .env(
            "WRIX_TEST_SERVICE_NAME",
            fixture.metadata["container_name"].as_str().unwrap(),
        )
        .output()?;
    assert_eq!(output.status.code(), Some(64));

    let name = fixture.metadata["container_name"].as_str().unwrap();
    assert!(
        fixture
            .runtime_command()?
            .args(["rm", "-f", name])
            .status()?
            .success()
    );
    let removed: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("runtime.json"))?)?;
    assert_eq!(removed, json!([]));
    assert!(
        fixture
            .runtime_command()?
            .args([
                "run",
                "-d",
                "--name",
                name,
                "image",
                "sh",
                "-c",
                "sleep infinity"
            ])
            .status()?
            .success()
    );
    let replacement: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("runtime.json"))?)?;
    assert_eq!(replacement, fixture.snapshot);
    Ok(())
}
