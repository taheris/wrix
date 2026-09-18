mod common;

use std::{fs, net::TcpListener, path::PathBuf, process::Command};

use common::{RunResult, TestResult, run_command, set_mode, wrix_command};
use serde_json::{Value, json};

const APPLE_RUNTIME: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${WRIX_TEST_RUNTIME_LOG:?}"
case "$*" in
  'list --all --format json') ;;
  "inspect ${WRIX_TEST_SERVICE_NAME:?}") ;;
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
                    "wrix.dolt.transport": "tcp"
                },
                "publishedPorts": [
                    {"hostAddress": "127.0.0.1", "hostPort": fixture.port("cache_http")?, "containerPort": 8080, "proto": "tcp", "count": 1},
                    {"hostAddress": "127.0.0.1", "hostPort": fixture.port("dolt_tcp")?, "containerPort": 3306, "proto": "tcp", "count": 1}
                ]
            },
            "status": {"state": "running"}
        }]);
        fixture.write_snapshot()?;
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
            .env(
                "WRIX_TEST_SERVICE_NAME",
                self.metadata["container_name"].as_str().unwrap_or("unused"),
            )
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE")
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE_KIND")
            .env_remove("WRIX_SERVICE_IMAGE_DIGEST");
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
    Ok(())
}
