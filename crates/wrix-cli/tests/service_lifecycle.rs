mod common;

use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use common::{RunResult, TestResult, run_command, set_mode, wrix_command};
use serde_json::{Value, json};

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

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    runtime: PathBuf,
    metadata: Value,
    snapshot: Value,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let root = tempfile::Builder::new().prefix("apple-service").tempdir()?;
        let workspace = root.path().join("workspace");
        let runtime = root.path().join("container");
        fs::create_dir_all(workspace.join(".beads/dolt"))?;
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
        let mut command = wrix_command(&self.workspace)?;
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
        Ok(serde_json::from_value(
            self.metadata["endpoints"][endpoint]["port"].clone(),
        )?)
    }

    fn state_root(&self) -> TestResult<PathBuf> {
        Ok(serde_json::from_value(self.metadata["state_root"].clone())?)
    }

    fn write_snapshot(&self) -> TestResult {
        fs::write(
            self.root.path().join("runtime.json"),
            serde_json::to_vec_pretty(&self.snapshot)?,
        )?;
        Ok(())
    }
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
