#[path = "common/celler.rs"]
mod celler;
mod common;
#[cfg(target_os = "linux")]
#[path = "common/component.rs"]
mod component;
#[path = "common/controller.rs"]
mod controller;
#[cfg(target_os = "linux")]
#[path = "common/maintenance.rs"]
mod maintenance;
#[path = "common/native_backend.rs"]
mod native_backend;
#[path = "common/native_celler.rs"]
mod native_celler;
#[path = "common/peer_runtime.rs"]
mod peer_runtime;
#[path = "common/service_binding.rs"]
mod service_binding;

use std::{
    fs,
    net::TcpListener,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use common::{RunResult, TestResult, host_wrix_command_with_path, run_command, set_mode};
use fs2::FileExt;
use serde_json::{Value, json};
use wrix_sandbox::command::Command as LaunchCommand;

const APPLE_RUNTIME: &str = concat!(
    include_str!("fixtures/shared-readiness.sh"),
    "\n",
    include_str!("fixtures/apple-service-runtime.sh")
);

#[test]
fn public_cli_shared_native_backend_preserves_namespaces_and_denies_peer_sql() -> TestResult {
    if std::env::var_os("WRIX_TEST_NATIVE_NETWORK").is_none() {
        let output = Command::new("timeout")
            .args([
                "--signal=TERM",
                "--kill-after=5",
                "240",
                "unshare",
                "--user",
                "--map-root-user",
                "--mount",
                "--net",
                "--propagation",
                "private",
                "sh",
                "-c",
                "ip link set lo up; if test -f /etc/wrix/image-agent; then mount -t tmpfs -o mode=755 tmpfs /etc/wrix; fi; exec \"$@\"",
                "fixture",
            ])
            .arg(std::env::current_exe()?)
            .args([
                "--exact",
                "public_cli_shared_native_backend_preserves_namespaces_and_denies_peer_sql",
                "--nocapture",
            ])
            .env("WRIX_TEST_NATIVE_NETWORK", "1")
            .output()?;
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    }
    let root = tempfile::Builder::new()
        .prefix("cli-native-")
        .tempdir_in(fs::canonicalize("/tmp")?)?;
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open("/tmp/wrix-shared-native-port.lock")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "native port fixture lock deadline exceeded"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error.into()),
        }
    }
    let bin = root.path().join("bin");
    fs::create_dir(&bin)?;
    for name in ["podman", "container"] {
        fs::write(
            bin.join(name),
            include_str!("../../wrix-service/tests/fixtures/shared-runtime.py"),
        )?;
        set_mode(&bin.join(name), 0o755)?;
    }
    native_backend::install(root.path())?;
    let native_bd = std::env::var_os("WRIX_REAL_BD_BIN")
        .unwrap_or_else(|| "/nix/store/7aalmix7lxvs73apxl1pwkrvqw52snhm-beads-1.3.1/bin/bd".into());
    let native_package = root.path().join("native-package");
    fs::create_dir_all(native_package.join("bin"))?;
    std::os::unix::fs::symlink(&native_bd, native_package.join("bin/bd"))?;
    let expression = format!(
        "import {} {{ pkgs = {{ writeShellScriptBin = name: script: script; beads = {}; }}; }}",
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../lib/beads/package.nix"),
        serde_json::to_string(&native_package)?
    );
    let rendered = Command::new(common::command_path("nix")?)
        .args([
            "eval",
            "--store",
            "dummy://",
            "--raw",
            "--impure",
            "--expr",
            &expression,
        ])
        .output()?;
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    fs::write(
        bin.join("bd"),
        [b"#!/usr/bin/env bash\n".as_slice(), &rendered.stdout].concat(),
    )?;
    set_mode(&bin.join("bd"), 0o755)?;
    service_binding::create(root.path(), &bin, "nix-descriptor")?;
    let _celler = celler::Server::watch(root.path(), None);
    let shared = common::binary_dir()?.join("wrix-shared-service");
    assert!(
        shared.is_file(),
        "build the native wrix-shared-service fixture before running CLI conformance"
    );
    #[cfg(target_os = "linux")]
    let controller = controller::Server::start_supervised(root.path())?;
    #[cfg(not(target_os = "linux"))]
    let controller = controller::Server::start(root.path())?;
    let mut command = Command::new("timeout");
    command
        .args(["--signal=TERM", "--kill-after=5", "240", "bash"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/host-command.sh"
        ))
        .arg("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/native-cli-shared.py"
        ))
        .arg(root.path())
        .arg(env!("CARGO_BIN_EXE_wrix"))
        .env("WRIX_SHARED_BINARY", shared)
        .env("WRIX_SHARED_FIXTURE", root.path())
        .env("HOME", root.path().join("home"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env(
            "PATH",
            std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
                &std::env::var_os("PATH").ok_or("missing PATH")?,
            )))?,
        )
        .env("WRIX_REAL_BD_BIN", native_bd)
        // The fixture owns its package binding, even in a bound host devshell.
        .env("WRIX_SERVICE_IMAGE", "wrix-fixture:ambient-conflict")
        .env("WRIX_SERVICE_IMAGE_SOURCE", "/must-not-read-ambient-image")
        .env("WRIX_SERVICE_IMAGE_SOURCE_KIND", "invalid-ambient-kind")
        .env("WRIX_SERVICE_IMAGE_DIGEST", "invalid-ambient-digest");
    common::set_isolated_git_env(&mut command);
    controller.configure(&mut command)?;
    let output = run_command(&mut command)?;
    assert!(
        output.status.success(),
        "{}\n{}",
        output.stdout,
        output.stderr
    );
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn cancel_unstarted_update_preserves_running_peers_and_allows_selected_service_reentry()
-> TestResult {
    let mut fixture = Fixture::shared_cache()?;
    let host = run_command(fixture.command()?.args(["service", "--host"]))?;
    assert!(host.status.success(), "{}", host.stderr);
    let deployment = host
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("deployment: "))
        .ok_or("missing deployment")?;
    let lifecycle = fixture.root.path().join("state/wrix/host-lifecycle");
    let pending = json!({
        "version": 2,
        "deployment": deployment,
        "attempt": "c".repeat(32),
        "candidate": serde_json::from_slice::<Value>(&fs::read(fixture.root.path().join("binding.json"))?)?,
        "controller": serde_json::from_slice::<Value>(&fs::read(fixture.root.path().join("state/wrix/control/package.json"))?)?,
        "pools": {"state": "unprovisioned"},
        "phase": "prepared"
    });
    let bytes = serde_json::to_vec(&pending)?;
    fs::write(lifecycle.join("update.json"), &bytes)?;
    set_mode(&lifecycle.join("update.json"), 0o600)?;
    fixture
        .snapshot
        .as_array_mut()
        .ok_or("missing inventory")?
        .push(json!({
            "configuration": {"id": "unrelated-worker", "labels": {"wrix.kind": "sandbox"}},
            "status": {"state": "running"}
        }));
    fixture.write_snapshot()?;
    let inventory = fs::read(fixture.root.path().join("runtime.json"))?;
    let payload = fixture
        .root
        .path()
        .join("state/wrix/service-data/retained-payload");
    fs::write(&payload, b"native data must survive cancellation")?;
    let signing_path = fixture.state_root()?.join("keys/cache.secret");
    let signing = fs::read(&signing_path)?;
    let owner = fs::read(lifecycle.join("celler-owner.json"))?;
    let blocked = fixture.run("start")?;
    assert!(!blocked.status.success());
    assert!(
        blocked.stderr.contains("pending service update"),
        "{}",
        blocked.stderr
    );
    let checkpoint = format!("host-lifecycle/checkpoints/{}", "c".repeat(32));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(lifecycle.join("checkpoints"))?;
    for marker in [
        "nix-pool-maintenance.json",
        "host-lifecycle/shutdown.json",
        &checkpoint,
    ] {
        let marker = fixture.root.path().join("state/wrix").join(marker);
        fs::write(&marker, b"uncertain maintenance")?;
        set_mode(&marker, 0o600)?;
        let refused = run_command(fixture.command()?.args(["service", "update", "--cancel"]))?;
        assert!(!refused.status.success());
        assert_eq!(fs::read(lifecycle.join("update.json"))?, bytes);
        assert_eq!(
            fs::read(fixture.root.path().join("runtime.json"))?,
            inventory
        );
        fs::remove_file(marker)?;
    }
    let unit = fixture
        .root
        .path()
        .join("state/wrix/control/wrix-host-controller.service");
    let original_unit = fs::read(&unit)?;
    fs::write(&unit, "foreign controller unit")?;
    let refused = run_command(fixture.command()?.args(["service", "update", "--cancel"]))?;
    assert!(!refused.status.success());
    assert_eq!(fs::read(lifecycle.join("update.json"))?, bytes);
    fs::write(unit, original_unit)?;
    fixture.snapshot[0]["status"]["state"] = json!("stopped");
    fixture.write_snapshot()?;
    let refused = run_command(fixture.command()?.args(["service", "update", "--cancel"]))?;
    assert!(!refused.status.success());
    assert_eq!(fs::read(lifecycle.join("update.json"))?, bytes);
    fixture.snapshot[0]["status"]["state"] = json!("running");
    fixture.write_snapshot()?;
    let cancel = run_command(fixture.command()?.args(["service", "update", "--cancel"]))?;
    assert!(cancel.status.success(), "{}", cancel.stderr);
    assert!(cancel.stdout.contains("Cancelled unstarted service update"));
    assert_eq!(
        fs::read(&payload)?,
        b"native data must survive cancellation"
    );
    assert_eq!(fs::read(signing_path)?, signing);
    assert_eq!(fs::read(lifecycle.join("celler-owner.json"))?, owner);
    assert!(!lifecycle.join("update.json").exists());
    assert_eq!(
        fs::read(lifecycle.join(format!("cancelled-update-{}.json", "c".repeat(32))))?,
        bytes
    );
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        inventory
    );
    let retry = run_command(fixture.command()?.args(["service", "update", "--cancel"]))?;
    assert!(retry.status.success(), "{}", retry.stderr);
    let start = fixture.run("start")?;
    assert!(start.status.success(), "{}", start.stderr);
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        inventory
    );
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn legacy_update_cancellation_requires_original_backend_and_controller_evidence() -> TestResult {
    let fixture = Fixture::legacy_shared_cache()?;
    let host = run_command(fixture.command()?.args(["service", "--host"]))?;
    assert!(host.status.success(), "{}", host.stderr);
    let deployment = host
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("deployment: "))
        .ok_or("missing deployment")?;
    let lifecycle = fixture.root.path().join("state/wrix/host-lifecycle");
    let pending = json!({
        "version": 1,
        "deployment": deployment,
        "attempt": "d".repeat(32),
        "candidate": serde_json::from_slice::<Value>(&fs::read(fixture.root.path().join("binding.json"))?)?,
        "controller": serde_json::from_slice::<Value>(&fs::read(fixture.root.path().join("state/wrix/control/package.json"))?)?,
        "pools": {"state": "unprovisioned"},
        "phase": "prepared"
    });
    let inventory = fs::read(fixture.root.path().join("runtime.json"))?;
    for (attempt, (version, phase, failure)) in [
        (1, "prepared", None),
        (1, "prepared", Some("backend_drain")),
        (2, "preparing", Some("backend_drain")),
    ]
    .into_iter()
    .enumerate()
    {
        for (backend_original, controller_original) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let mut pending = pending.clone();
            pending["version"] = json!(version);
            pending["phase"] = json!(phase);
            pending["failure"] = json!(failure);
            pending["attempt"] = json!(format!("{attempt:032x}"));
            if backend_original {
                pending["candidate"]["image"] = json!("localhost/other:bound");
            }
            if controller_original {
                pending["controller"]["controller"] =
                    json!("/nix/store/11111111111111111111111111111111-controller");
            }
            let cancellable = backend_original && controller_original;
            let bytes = serde_json::to_vec(&pending)?;
            fs::write(lifecycle.join("update.json"), &bytes)?;
            set_mode(&lifecycle.join("update.json"), 0o600)?;
            let cancel = run_command(fixture.command()?.args(["service", "update", "--cancel"]))?;
            assert_eq!(cancel.status.success(), cancellable, "{}", cancel.stderr);
            if cancellable {
                assert_eq!(
                    fs::read(lifecycle.join(format!("cancelled-update-{attempt:032x}.json")))?,
                    bytes
                );
                let resumed = fixture.run("start")?;
                assert!(resumed.status.success(), "{}", resumed.stderr);
            } else {
                assert!(cancel.stderr.contains("lacks proof"), "{}", cancel.stderr);
                assert_eq!(fs::read(lifecycle.join("update.json"))?, bytes);
            }
            assert_eq!(
                fs::read(fixture.root.path().join("runtime.json"))?,
                inventory
            );
        }
    }
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn legacy_prepared_update_with_native_generations_remains_fenced() -> TestResult {
    let fixture = Fixture::shared_cache()?;
    let host = run_command(fixture.command()?.args(["service", "--host"]))?;
    let deployment = host
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("deployment: "))
        .ok_or("missing deployment")?;
    let lifecycle = fixture.root.path().join("state/wrix/host-lifecycle");
    let mut candidate: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("binding.json"))?)?;
    candidate["image"] = json!("localhost/other:bound");
    let mut controller: Value = serde_json::from_slice(&fs::read(
        fixture.root.path().join("state/wrix/control/package.json"),
    )?)?;
    controller["controller"] = json!("/nix/store/11111111111111111111111111111111-controller");
    let pending = serde_json::to_vec(&json!({
        "version": 1, "deployment": deployment, "attempt": "e".repeat(32),
        "candidate": candidate, "controller": controller,
        "pools": {"state": "unprovisioned"}, "phase": "prepared"
    }))?;
    fs::write(lifecycle.join("update.json"), &pending)?;
    set_mode(&lifecycle.join("update.json"), 0o600)?;
    let plan = fixture
        .root
        .path()
        .join("state/wrix/control/authority/native-plan.json");
    let native = fs::read(&plan)?;
    let inventory = fs::read(fixture.root.path().join("runtime.json"))?;
    let refused = run_command(fixture.command()?.args(["service", "update", "--cancel"]))?;
    assert!(!refused.status.success());
    assert!(refused.stderr.contains("lacks proof"), "{}", refused.stderr);
    assert_eq!(fs::read(lifecycle.join("update.json"))?, pending);
    assert_eq!(fs::read(plan)?, native);
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        inventory
    );
    Ok(())
}

#[test]
fn shared_service_capabilities_are_passive_without_workspace_or_runtime() -> TestResult {
    let root = tempfile::tempdir()?;
    let output = Command::new(env!("CARGO_BIN_EXE_wrix"))
        .args(["service", "capabilities"])
        .current_dir(root.path())
        .env("HOME", root.path().join("home"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("WRIX_CONTAINER_RUNTIME", "/missing/runtime")
        .env("WRIX_SERVICE_PACKAGE_BINDING", "/missing/binding")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"shared-services-v1\n");
    assert_eq!(output.stderr, b"");
    assert!(fs::read_dir(root.path())?.next().is_none());
    Ok(())
}

#[test]
fn running_cache_reentry_restores_only_the_verifier_from_existing_private_authority() -> TestResult
{
    let fixture = Fixture::shared_cache()?;
    let state = fixture.state_root()?;
    let host = state.parent().unwrap().parent().unwrap();
    let issuer = host.join("host-control/celler-issuer/issuer.pem");
    let verifier = host.join("service-data/celler/jwt.pub");
    let private = fs::read(&issuer)?;
    let public = fs::read(&verifier)?;
    fs::remove_file(&verifier)?;
    for _ in 0..2 {
        let output = fixture.run("start")?;
        assert!(output.status.success(), "{}", output.stderr);
        assert_eq!(fs::read(&issuer)?, private);
        assert_eq!(fs::read(&verifier)?, public);
    }
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn reentry_preserves_host_selected_finite_publication_retention() -> TestResult {
    let fixture = Fixture::shared_cache()?;
    let state = fixture.state_root()?;
    let host = state.parent().unwrap().parent().unwrap();
    let workspace = wrix_core::path::Workspace::from_path(&fixture.workspace)?;
    wrix_cache::backend::publication::Publisher::install_issuer(
        &state,
        workspace.hash(),
        &format!("http://127.0.0.1:{}/", fixture.port("cache_http")?),
        wrix_cache::backend::publication::registration::private_namespace(workspace.hash())?,
        &host.join("host-control/celler-issuer"),
        Duration::from_hours(168),
    )?;
    let output = fixture.run("start")?;
    assert!(output.status.success(), "{}", output.stderr);
    let active: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("celler-policy.json"))?)?;
    assert_eq!(active["retention_period"]["Period"], 7 * 24 * 60 * 60);
    let publisher = wrix_cache::backend::publication::Publisher::load(&state, workspace.hash())?;
    assert_eq!(publisher.retention(), Duration::from_hours(168));
    Ok(())
}

#[test]
fn running_cache_owner_cannot_bootstrap_replacement_issuer_authority() -> TestResult {
    let fixture = Fixture::shared_cache()?;
    let state = fixture.state_root()?;
    let host = state.parent().unwrap().parent().unwrap();
    let issuer = host.join("host-control/celler-issuer/issuer.pem");
    let verifier = host.join("service-data/celler/jwt.pub");
    let before = fs::read(&verifier)?;
    let configuration = host.join("service-data/celler/server.toml");
    let configuration_before = fs::read(&configuration)?;
    fs::remove_file(&issuer)?;
    let output = fixture.run("start")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("host-private Celler issuer"),
        "{}",
        output.stderr
    );
    assert!(!issuer.exists());
    assert_eq!(fs::read(verifier)?, before);
    assert_eq!(fs::read(configuration)?, configuration_before);
    assert!(!fixture.root.path().join("run.argv").exists());
    let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert!(!calls.lines().any(|line| line.starts_with("rm ")));
    Ok(())
}

#[test]
fn service_fixture_keeps_native_host_state_private() -> TestResult {
    let fixture = Fixture::new()?;
    let state_root = fixture.state_root()?;
    let native_root = state_root
        .parent()
        .and_then(Path::parent)
        .ok_or("missing native root")?;
    assert_eq!(common::mode(native_root)?, 0o700);
    assert_eq!(common::mode(&state_root.join("keys"))?, 0o700);
    assert_eq!(common::mode(&native_root.join("state.db"))?, 0o600);
    let imported: Value = serde_json::from_slice(&common::read_namespace_document(
        &state_root.join("services.json"),
    )?)?;
    assert_eq!(imported, fixture.metadata);
    Ok(())
}

#[test]
fn malformed_sync_branch_fails_before_service_runtime_or_layout() -> TestResult {
    let fixture = Fixture::new()?;
    let state_root = fixture.state_root()?;
    let before = common::read_namespace_document(&state_root.join("services.json"))?;
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
    assert_eq!(
        common::read_namespace_document(&state_root.join("services.json"))?,
        before
    );
    Ok(())
}

struct Fixture {
    controller: std::cell::RefCell<Option<controller::Server>>,
    server: std::cell::RefCell<Option<celler::Server>>,
    reservation: std::cell::RefCell<Option<TcpListener>>,
    #[cfg(target_os = "linux")]
    component: Option<component::Server>,
    root: tempfile::TempDir,
    workspace: PathBuf,
    runtime: PathBuf,
    metadata: Value,
    snapshot: Value,
}

#[test]
#[cfg(target_os = "linux")]
fn supervised_controller_fixture_obeys_native_ownership_and_rejects_unknown_manager_commands()
-> TestResult {
    use wrix_service::control::supervision::{Lifecycle, Native, Package, Platform};
    let fixture = Fixture::new()?;
    let package = Package::load(std::path::Path::new(
        &std::env::var_os("WRIX_TEST_CONTROLLER_BINDING")
            .ok_or("missing controller fixture binding")?,
    ))?;
    let control = fixture.root.path().join("state/wrix/control");
    let manager = fixture.root.path().join("controller-supervisor");
    let native = Native::new(
        Platform::Linux,
        rustix::process::geteuid().as_raw(),
        manager.clone(),
        fixture.root.path().join("controller-pin"),
    );
    assert_eq!(
        Lifecycle::lock(&control)?.preflight(&native, &package)?,
        Some(package)
    );
    assert!(
        !Command::new(manager)
            .args(["--user", "unknown-operation"])
            .status()?
            .success()
    );
    Ok(())
}

impl Fixture {
    /// A cache-only namespace on a fresh, verified singleton deployment. SQL
    /// namespaces instead use the native public-CLI conformance fixture above.
    fn shared_cache() -> TestResult<Self> {
        let mut fixture = Self::new()?;
        fixture.disable_dolt()?;
        fixture.snapshot = json!([]);
        fixture.write_snapshot()?;
        let output = fixture.run("start")?;
        assert!(output.status.success(), "{}", output.stderr);
        fixture.snapshot =
            serde_json::from_slice(&fs::read(fixture.root.path().join("runtime.json"))?)?;
        let output = fixture.run("endpoints")?;
        assert!(output.status.success(), "{}", output.stderr);
        fixture.metadata = serde_json::from_str(&output.stdout)?;
        // Preserve the NUL runtime contract record for readiness checks, while
        // clearing the old line-oriented creation sentinel for reentry tests.
        fs::remove_file(fixture.root.path().join("run.argv"))?;
        fs::remove_file(fixture.root.path().join("runtime.log"))?;
        Ok(fixture)
    }

    /// Retained pre-generation shared-backend metadata, admitted by current startup.
    #[cfg(target_os = "linux")]
    fn legacy_shared_cache() -> TestResult<Self> {
        let mut fixture = Self::new()?;
        fixture.disable_dolt()?;
        let digest = format!("sha256:{}", "a".repeat(64));
        fixture.snapshot[0]["configuration"]["image"] = json!({"descriptor": {"digest": digest}});
        fixture.snapshot[0]["configuration"]["labels"]["wrix.service.supervision"] = json!("2");
        fixture.snapshot[0]["configuration"]["labels"]["wrix.dolt.shared"] = json!("native-v1");
        fixture.write_snapshot()?;
        let root = fixture.root.path().join("state/wrix/host-lifecycle");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
        let binding: Value =
            serde_json::from_slice(&fs::read(fixture.root.path().join("binding.json"))?)?;
        for (name, document) in [
            (
                "celler-owner.json",
                json!({
                    "schema_version": 2, "backend_version": "celler-v2", "runtime": "container",
                    "workspace_hash": fixture.metadata["workspace_hash"],
                    "container_name": fixture.metadata["container_name"],
                    "cache_http_port": fixture.port("cache_http")?,
                    "recipe": {"binding": binding, "image_id": digest, "config_digest": digest}
                }),
            ),
            (
                "shared.json",
                json!({"version":1, "tcp":true, "sql_port": fixture.port("dolt_tcp")?, "sync_port": 3307}),
            ),
        ] {
            fs::write(root.join(name), serde_json::to_vec(&document)?)?;
            set_mode(&root.join(name), 0o600)?;
        }
        let data = fixture.root.path().join("state/wrix/service-data/dolt");
        fs::create_dir_all(&data)?;
        let retained_launch = [
            "--name".to_owned(),
            fixture.metadata["container_name"]
                .as_str()
                .ok_or("missing container")?
                .to_owned(),
            "--label".into(),
            "wrix.dolt.shared=native-v1".into(),
            "-v".into(),
            format!("{}:{}:rw", data.display(), data.display()),
        ];
        let mut launch = retained_launch.join("\0").into_bytes();
        launch.push(0);
        fs::write(fixture.root.path().join("run.argv.nul"), launch)?;
        let started = fixture.run("start")?;
        assert!(started.status.success(), "{}", started.stderr);
        assert!(
            !fixture
                .root
                .path()
                .join("state/wrix/control/authority/native-plan.json")
                .exists()
        );
        Ok(fixture)
    }

    fn new() -> TestResult<Self> {
        // Repository-local TMPDIR paths inherit the enclosing checkout's service identity.
        let root = tempfile::Builder::new()
            .prefix("apple-service")
            .tempdir_in(fs::canonicalize("/tmp")?)?;
        Self::from_root(root)
    }

    fn from_root(root: tempfile::TempDir) -> TestResult<Self> {
        Self::from_root_at(root, "")
    }

    fn from_root_at(root: tempfile::TempDir, layout: &str) -> TestResult<Self> {
        let outer = root.path().join("workspace");
        fs::create_dir_all(&outer)?;
        initialize_repository(&outer)?;
        let workspace = outer.join(layout);
        let runtime = root.path().join("container");
        fs::create_dir_all(workspace.join(".beads/dolt"))?;
        initialize_repository(&workspace)?;
        fs::write(&runtime, APPLE_RUNTIME)?;
        set_mode(&runtime, 0o755)?;
        service_binding::create(root.path(), &root.path().join("bin"), "docker-archive")?;
        fs::copy(&runtime, root.path().join("bin/container"))?;
        peer_runtime::install_empty(&root.path().join("bin"))?;
        let mut fixture = Self {
            controller: std::cell::RefCell::new(None),
            server: std::cell::RefCell::new(None),
            reservation: std::cell::RefCell::new(None),
            #[cfg(target_os = "linux")]
            component: None,
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
        let listener =
            TcpListener::bind(("127.0.0.1", fixture.port("cache_http")?)).or_else(|error| {
                if error.kind() == std::io::ErrorKind::AddrInUse {
                    TcpListener::bind(("127.0.0.1", 0))
                } else {
                    Err(error)
                }
            })?;
        fixture.metadata["endpoints"]["cache_http"]["port"] = json!(listener.local_addr()?.port());
        *fixture.reservation.borrow_mut() = Some(listener);
        fixture.snapshot = json!([{
            "configuration": {
                "id": fixture.metadata["container_name"],
                "labels": {
                    "wrix.kind": "service",
                    "wrix.workspace": fixture.metadata["workspace_path"],
                    "wrix.workspace.hash": fixture.metadata["workspace_hash"],
                    "wrix.cache.enabled": "true",
                    "wrix.cache.backend": "celler-v2",
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
        service_binding::provision_celler_authority(fixture.root.path())?;
        let state_root = fixture.state_root()?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(state_root.join("keys"))?;
        fs::write(
            state_root.join("services.json"),
            serde_json::to_vec_pretty(&fixture.metadata)?,
        )?;
        fs::write(state_root.join("keys/cache.secret"), "fixture-secret\n")?;
        fs::write(
            state_root.join("keys/cache.pub"),
            "wrix-cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
        )?;
        common::initialize_host_scope(fixture.root.path())?;
        common::import_host_namespace(
            fixture.root.path(),
            fixture.metadata["workspace_hash"]
                .as_str()
                .ok_or("missing workspace hash")?,
        )?;
        #[cfg(target_os = "linux")]
        {
            *fixture.controller.borrow_mut() =
                Some(controller::Server::start_supervised(fixture.root.path())?);
        }
        Ok(fixture)
    }

    fn command(&self) -> TestResult<Command> {
        let mut command =
            host_wrix_command_with_path(&self.workspace, &[&self.root.path().join("bin")])?;
        command
            .env(
                "WRIX_SERVICE_PACKAGE_BINDING",
                self.root.path().join("binding.json"),
            )
            .env("WRIX_TEST_DIGEST", "a".repeat(64))
            .env("WRIX_TEST_JQ_BIN", common::command_path("jq")?)
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
                "WRIX_TEST_COMPONENT_BIN",
                env!("CARGO_BIN_EXE_wrix-service-supervisor"),
            )
            .env(
                "WRIX_TEST_COMPONENT_ROOT",
                self.root.path().join("component-runtime"),
            )
            .env(
                "WRIX_TEST_RUNTIME_REPLACEMENT",
                self.root.path().join("replacement.json"),
            )
            .env(
                "WRIX_TEST_SERVICE_NAME",
                self.metadata["container_name"].as_str().unwrap_or("unused"),
            )
            .env_remove("WRIX_SERVICE_IMAGE")
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE")
            .env_remove("WRIX_SERVICE_IMAGE_SOURCE_KIND")
            .env_remove("WRIX_SERVICE_IMAGE_DIGEST");
        if let Some(controller) = self.controller.borrow().as_ref() {
            controller.configure(&mut command)?;
        }
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

    #[cfg(target_os = "linux")]
    fn enable_components(&mut self) -> TestResult {
        if self.component.is_some() {
            return Err("component fixture already running".into());
        }
        self.snapshot[0]["configuration"]["labels"]["wrix.service.supervision"] = json!("2");
        self.write_snapshot()?;
        self.component = Some(component::Server::start(
            &self.root.path().join("component-runtime"),
            self.metadata["workspace_hash"]
                .as_str()
                .ok_or("missing component namespace")?,
        )?);
        Ok(())
    }

    fn api(&self) -> TestResult {
        if self.server.borrow().is_some() {
            return Ok(());
        }
        let running_modern = match serde_json::from_slice::<Vec<Value>>(&fs::read(
            self.root.path().join("runtime.json"),
        )?) {
            Ok(inventory) => inventory
                .iter()
                .find(|snapshot| snapshot["configuration"]["id"] == self.metadata["container_name"])
                .is_some_and(|snapshot| {
                    let labels = &snapshot["configuration"]["labels"];
                    snapshot["status"]["state"] == "running"
                        && labels["wrix.cache.backend"] == "celler-v2"
                        && labels["wrix.cache.enabled"] == "true"
                        && labels["wrix.service.supervision"]
                            .as_str()
                            .is_some_and(|version| matches!(version, "1" | "2"))
                        && (labels["wrix.dolt.transport"] == "disabled"
                            || labels["wrix.dolt.auth"] == "tcp-root-v1")
                }),
            Err(error) => {
                tracing::debug!(%error, "malformed external inventory fixture cannot serve cache requests");
                false
            }
        };
        let listener = self.reservation.borrow_mut().take();
        *self.server.borrow_mut() = Some(if running_modern {
            celler::Server::reserved(
                self.root.path(),
                listener.ok_or("running fixture lost its port reservation")?,
            )?
        } else {
            drop(listener);
            celler::Server::watch(self.root.path(), None)
        });
        Ok(())
    }

    fn run(&self, operation: &str) -> TestResult<RunResult> {
        if matches!(operation, "start" | "recover") {
            self.api()?;
        }
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
        self.api()?;
        if self.controller.borrow().is_none() {
            *self.controller.borrow_mut() = Some(controller::Server::start(self.root.path())?);
        }
        let bin = self.root.path().join("bin");
        fs::create_dir_all(&bin)?;
        for name in ["podman", "container", "route", "skopeo"] {
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
                "system": "x86_64-linux",
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
        self.controller
            .borrow()
            .as_ref()
            .ok_or("missing fixture controller")?
            .configure(&mut command)?;
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
        .prefix("outer-")
        .tempdir_in(fs::canonicalize("/tmp")?)?;
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
        .tempdir_in(fs::canonicalize("/tmp")?)?;
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
    let fixture = Fixture::shared_cache()?;
    let selected = owner_selection(&fixture)?;
    let shared: Value =
        serde_json::from_slice(&fs::read(selected.parent().unwrap().join("shared.json"))?)?;
    let sql_port: u16 = serde_json::from_value(shared["sql_port"].clone())?;
    let _sql = std::net::TcpStream::connect(("127.0.0.1", sql_port))?;

    for _ in 0..2 {
        let output = fixture.run("start")?;
        assert!(output.status.success(), "{}", output.stderr);
        assert!(
            output.stdout.contains("runtime: Running"),
            "{}",
            output.stdout
        );
        let persisted: Value = serde_json::from_slice(&common::read_namespace_document(
            &fixture.state_root()?.join("services.json"),
        )?)?;
        assert_eq!(persisted["endpoints"], fixture.metadata["endpoints"]);
        let exported: Value = serde_json::from_str(&fixture.run("endpoints")?.stdout)?;
        assert_eq!(exported["endpoints"], fixture.metadata["endpoints"]);
    }
    let log = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert!(
        log.lines().all(|line| line.starts_with("list ")
            || line.starts_with("inspect ")
            || line.starts_with("exec ")),
        "{log}"
    );
    Ok(())
}

#[test]
fn apple_start_preserves_stopped_legacy_tcp_services_pending_reconciliation() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.snapshot[0]["status"]["state"] = json!("stopped");
    fixture.snapshot[0]["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.cache.backend");
    fixture.snapshot[0]["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.dolt.auth");
    fixture.write_snapshot()?;
    let marker = fixture.workspace.join(".beads/dolt/keep");
    fs::write(&marker, "persistent database")?;

    assert_cache_start_preserves_owner(&fixture)?;
    assert_eq!(fs::read_to_string(marker)?, "persistent database");
    Ok(())
}

#[test]
fn fresh_service_refuses_empty_dolt_source_without_discarding_it() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.path().join("runtime.json"), "[]")?;
    let source = fixture.workspace.join(".beads/dolt");
    let marker = source.join("retained-evidence");
    fs::write(&marker, "not a native database")?;
    let output = fixture.run("start")?;
    assert!(!output.status.success());
    assert!(
        output
            .stderr
            .contains("no configured native Beads database was found"),
        "{}",
        output.stderr
    );
    assert_eq!(fs::read_to_string(marker)?, "not a native database");
    let log = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("run -d --name wrix-host-service "))
            .count(),
        1
    );
    assert!(!log.lines().any(|line| line.starts_with("rm ")));
    // Genuine source adoption, scoped SQL, and singleton preservation are
    // verified by public_cli_shared_native_backend_preserves_namespaces_and_denies_peer_sql.
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
    let before = common::read_namespace_document(&persisted)?;
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
    assert_eq!(common::read_namespace_document(&persisted)?, before);
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
fn host_policy_decoder_consumes_real_service_endpoint_metadata() -> TestResult {
    let fixture = Fixture::new()?;
    let output = fixture.run("endpoints")?;
    assert!(output.status.success(), "{}", output.stderr);
    let endpoint =
        wrix_cache::host_policy::cache_endpoint(&output.stdout)?.ok_or("cache endpoint missing")?;
    assert_eq!(
        endpoint.ip(),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    );
    assert_eq!(endpoint.port(), fixture.port("cache_http")?);
    Ok(())
}

#[test]
fn cache_launch_admits_the_provided_host_package_before_service_effects() -> TestResult {
    for mode in [LaunchCommand::Run, LaunchCommand::Spawn] {
        let fixture = Fixture::new()?;
        let mut command = fixture.launch_command(mode, &fixture.workspace)?;
        command.env_remove("WRIX_CONTROLLER_PACKAGE_BINDING");
        let before = fs::read(fixture.state_root()?.join("services.json"))?;
        let output = run_command(&mut command)?;
        assert!(!output.status.success());
        assert!(output.stderr.contains("provide a compatible host wrix"));
        assert!(!fixture.root.path().join("runtime.log").exists());
        assert!(!fixture.root.path().join("argv").exists());
        assert!(!fixture.workspace.join(".wrix/log").exists());
        assert_eq!(
            fs::read(fixture.state_root()?.join("services.json"))?,
            before
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires host capability; run in the disposable native-system guest"]
fn workspace_stop_retires_current_native_read_issuance_without_stopping_controller() -> TestResult {
    use wrix_service::control::{Client, Namespace, Operation, Reply};
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    let output =
        run_command(&mut fixture.launch_command(LaunchCommand::Spawn, &fixture.workspace)?)?;
    assert!(output.status.success(), "{}", output.stderr);
    let state = fixture.state_root()?;
    let client = Client::open(
        &state
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("control/authority"),
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let namespace =
        Namespace::from(wrix_core::path::Workspace::from_service_path(&fixture.workspace)?.hash());
    let Reply::Snapshot { namespaces, .. } =
        runtime.block_on(client.request(Operation::Inspect {}))?
    else {
        return Err("missing control snapshot".into());
    };
    assert!(namespaces.contains_key(&namespace));
    let stopped = fixture.run("stop")?;
    assert!(stopped.status.success(), "{}", stopped.stderr);
    let Reply::Snapshot { namespaces, .. } =
        runtime.block_on(client.request(Operation::Inspect {}))?
    else {
        return Err("controller stopped with workspace".into());
    };
    assert!(!namespaces.contains_key(&namespace));
    assert!(
        runtime
            .block_on(client.cache_credential(namespace))
            .is_err()
    );
    Ok(())
}

#[test]
#[ignore = "requires host capability; run in the disposable native-system guest"]
fn cache_guest_endpoint_and_key_reach_both_launch_commands() -> TestResult {
    for mode in [LaunchCommand::Run, LaunchCommand::Spawn] {
        for layout in ["", ".loom/beads/cache-route", ".loom/integration"] {
            let mut fixture =
                Fixture::from_root_at(tempfile::tempdir_in(fs::canonicalize("/tmp")?)?, layout)?;
            fixture.disable_dolt()?;
            let workspace = fixture.workspace.clone();
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
            let canonical = wrix_core::path::Workspace::from_service_path(&workspace)?;
            let views = wrix_cache::backend::pull::ReadSet::for_workspace(canonical.hash())?;
            let common_key = fs::read_to_string(
                fixture
                    .state_root()?
                    .parent()
                    .ok_or("missing namespace parent")?
                    .join("common/keys/cache.pub"),
            )?;
            assert_eq!(
                env.iter()
                    .filter(|value| value.starts_with("NIX_CONFIG="))
                    .copied()
                    .collect::<Vec<_>>(),
                [format!(
                    "NIX_CONFIG=extra-substituters = http://192.168.64.12:8080/{} http://192.168.64.12:8080/{}\nextra-trusted-public-keys = {} {}\nbuilders-use-substitutes = true\nnetrc-file = /run/wrix/cache-credentials/netrc\nnarinfo-cache-negative-ttl = 0",
                    views.common(),
                    views.private(),
                    common_key.trim(),
                    public_key.trim()
                )]
            );
            let directories = argv
                .windows(2)
                .filter(|pair| pair[0] == "-v")
                .filter_map(|pair| pair[1].strip_suffix(":/run/wrix/cache-credentials:ro"))
                .collect::<Vec<_>>();
            assert_eq!(directories.len(), 1);
            assert!(!Path::new(directories[0]).exists());
            assert!(!argv.iter().any(|arg| arg.contains("eyJhbGciOiJ")));
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
            let persisted: Value = serde_json::from_slice(&common::read_namespace_document(
                &state_root.join("services.json"),
            )?)?;
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
#[ignore = "requires host capability; run in the disposable native-system guest"]
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
fn explicit_cache_rotation_refuses_unverified_selected_owner_before_key_mutation() -> TestResult {
    let mut fixture = Fixture::shared_cache()?;
    let root = fixture.state_root()?;
    let before = fs::read(root.join("keys/cache.pub"))?;
    fixture.snapshot[0]["configuration"]["labels"]["wrix.service.scope"] = json!("foreign");
    fixture.write_snapshot()?;
    let output = run_command(fixture.command()?.args(["service", "cache", "rotate-key"]))?;
    assert!(!output.status.success());
    assert!(output.stderr.contains("ownership"), "{}", output.stderr);
    assert_eq!(fs::read(root.join("keys/cache.pub"))?, before);
    assert!(!root.join("celler-rotation.json").exists());
    Ok(())
}

#[test]
fn explicit_cache_rotation_restores_missing_publication_binding_after_key_conflict() -> TestResult {
    let fixture = Fixture::shared_cache()?;
    let root = fixture.state_root()?;
    drop(fixture.server.borrow_mut().take());
    let native: Value = serde_json::from_slice(&fs::read(
        fixture
            .root
            .path()
            .join("state/wrix/control/authority/native-plan.json"),
    )?)?;
    let _native = native_celler::Server::start(
        fixture.root.path(),
        serde_json::from_value(native["backend"]["cache"].clone())?,
        fixture.port("cache_http")?,
    )?;
    let workspace = wrix_core::path::WorkspaceHash::parse(
        fixture.metadata["workspace_hash"]
            .as_str()
            .ok_or("missing workspace hash")?,
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let publisher = wrix_cache::backend::publication::Publisher::load(&root, &workspace)?;
    let original_backend_key = runtime.block_on(publisher.register())?;
    let binding: Value = serde_json::from_slice(&fs::read(root.join("celler-publisher.json"))?)?;
    let common_key = runtime.block_on(
        wrix_cache::backend::publication::registration::provision_common(
            &fixture.root.path().join("native-common-bootstrap"),
            binding["endpoint"].as_str().ok_or("missing endpoint")?,
            Path::new(
                binding["issuer_root"]
                    .as_str()
                    .ok_or("missing issuer path")?,
            ),
            Duration::from_hours(720),
            Duration::from_secs(1),
        ),
    )?;
    fs::write(
        root.parent()
            .ok_or("missing state base")?
            .join("common/keys/cache.pub"),
        common_key.as_str(),
    )?;
    let public = root.join("keys/cache.pub");
    let retained = "retained:PUAXw+hDiVqStwqnTRt+vJyYLM8uxJaMwM1V8Sr0Zgw=";
    fs::write(&public, retained)?;
    fs::remove_file(root.join("celler-publisher.json"))?;
    let output = run_command(fixture.command()?.args(["service", "start"]))?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("explicit key reconciliation"),
        "{}",
        output.stderr
    );
    assert_eq!(fs::read_to_string(&public)?, retained);
    let secret = fs::read(root.join("keys/cache.secret"))?;
    let output = run_command(fixture.command()?.args(["service", "cache", "rotate-key"]))?;
    assert!(output.status.success(), "{}", output.stderr);
    let key_text = fs::read_to_string(&public)?;
    let key = wrix_core::cache_key::CachePublicKey::parse(key_text.trim())?;
    assert_ne!(key, original_backend_key);
    let publisher = wrix_cache::backend::publication::Publisher::load(&root, &workspace)?;
    assert_eq!(runtime.block_on(publisher.register())?, key);
    assert_eq!(fs::read(root.join("keys/cache.secret"))?, secret);
    assert!(!root.join("celler-rotation.json").exists());
    let output = run_command(fixture.command()?.args(["service", "start"]))?;
    assert!(output.status.success(), "{}", output.stderr);
    Ok(())
}

#[test]
fn cache_start_preserves_valid_keys_and_requires_explicit_legacy_conversion() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.snapshot[0]["status"]["state"] = json!("stopped");
    fixture.disable_dolt()?;
    fixture.snapshot[0]["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.cache.backend");
    fixture.write_snapshot()?;
    let key = fs::read(fixture.state_root()?.join("keys/cache.pub"))?;
    let before = fs::read(fixture.root.path().join("runtime.json"))?;
    assert_cache_start_preserves_owner(&fixture)?;
    assert_eq!(fs::read(fixture.root.path().join("runtime.json"))?, before);
    assert_eq!(fs::read(fixture.state_root()?.join("keys/cache.pub"))?, key);
    Ok(())
}

#[test]
fn ordinary_start_preserves_running_services_with_unverified_features() -> TestResult {
    for label in [
        "wrix.cache.backend",
        "wrix.service.supervision",
        "wrix.dolt.auth",
    ] {
        let mut fixture = Fixture::new()?;
        fixture.snapshot[0]["configuration"]["labels"]
            .as_object_mut()
            .unwrap()
            .remove(label);
        fixture.write_snapshot()?;
        let marker = fixture.workspace.join(".beads/dolt/keep");
        fs::write(&marker, "persistent database")?;
        assert_cache_start_preserves_owner(&fixture)?;
        assert_eq!(fs::read_to_string(marker)?, "persistent database");
    }
    Ok(())
}

#[test]
fn service_start_never_removes_workers_with_the_same_workspace_identity() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    let mut worker = fixture.snapshot[0].clone();
    worker["configuration"]["id"] = json!("active-worker");
    worker["configuration"]["labels"]["wrix.kind"] = json!("sandbox");
    worker["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.cache.backend");
    worker["configuration"]["publishedPorts"] = json!([]);
    fixture.snapshot.as_array_mut().unwrap().push(worker);
    fixture.write_snapshot()?;
    let before = fs::read(fixture.root.path().join("runtime.json"))?;
    assert_cache_start_preserves_owner(&fixture)?;
    assert_eq!(fs::read(fixture.root.path().join("runtime.json"))?, before);
    let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert!(!calls.lines().any(|line| line.starts_with("rm ")));
    Ok(())
}

#[test]
fn obsolete_running_service_is_not_removed_during_namespace_startup() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    let mut obsolete = fixture.snapshot[0].clone();
    obsolete["configuration"]["id"] = json!("older-service-name");
    obsolete["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.cache.backend");
    fixture.snapshot.as_array_mut().unwrap().push(obsolete);
    fixture.write_snapshot()?;
    assert_cache_start_preserves_owner(&fixture)?;
    Ok(())
}

#[test]
fn stopped_native_celler_is_not_removed_by_namespace_stop() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fixture.snapshot[0]["status"]["state"] = json!("stopped");
    fixture.write_snapshot()?;
    let before = fs::read(fixture.root.path().join("runtime.json"))?;
    let key = fs::read(fixture.state_root()?.join("keys/cache.pub"))?;
    let output = fixture.run("stop")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("host-wide lifecycle integration"),
        "{}",
        output.stderr
    );
    assert_eq!(fs::read(fixture.root.path().join("runtime.json"))?, before);
    assert_eq!(fs::read(fixture.state_root()?.join("keys/cache.pub"))?, key);
    let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert!(!calls.lines().any(|line| line.starts_with("rm ")));
    Ok(())
}

#[test]
fn all_obsolete_services_are_validated_before_any_container_is_removed() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    for (name, transport) in [
        ("stopped-legacy-cache", "disabled"),
        ("stopped-dolt", "tcp"),
    ] {
        let mut obsolete = fixture.snapshot[0].clone();
        obsolete["configuration"]["id"] = json!(name);
        obsolete["status"]["state"] = json!("stopped");
        obsolete["configuration"]["labels"]
            .as_object_mut()
            .unwrap()
            .remove("wrix.cache.backend");
        obsolete["configuration"]["labels"]["wrix.dolt.transport"] = json!(transport);
        fixture.snapshot.as_array_mut().unwrap().push(obsolete);
    }
    fixture.write_snapshot()?;
    assert_cache_start_preserves_owner(&fixture)?;
    fixture.snapshot[2]["configuration"]["labels"]["wrix.dolt.transport"] = json!("disabled");
    fixture.write_snapshot()?;
    // Even cache-only stopped peers are not automatic replacement authority.
    // Ordinary startup leaves the entire legacy inventory for explicit recovery.
    assert_cache_start_preserves_owner(&fixture)?;
    let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert!(!calls.lines().any(|line| line.starts_with("rm ")));
    assert!(!calls.lines().any(|line| line.starts_with("run ")));
    Ok(())
}

#[test]
fn a_foreign_hash_is_not_overridden_by_a_matching_workspace_path() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fixture.snapshot[0]["configuration"]["labels"]["wrix.workspace.hash"] = json!("f".repeat(64));
    fixture.snapshot[0]["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.cache.backend");
    fixture.write_snapshot()?;
    assert_cache_start_preserves_owner(&fixture)?;
    let before = fs::read(fixture.root.path().join("runtime.json"))?;
    let output = fixture.run("stop")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("service migrate-legacy"),
        "{}",
        output.stderr
    );
    assert_eq!(fs::read(fixture.root.path().join("runtime.json"))?, before);
    Ok(())
}

#[test]
fn cache_start_preserves_running_backend_without_lifetime_ownership_guard() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fixture.snapshot[0]["configuration"]["labels"]["wrix.cache.backend"] = json!("celler-v1");
    fixture.write_snapshot()?;
    assert_cache_start_preserves_owner(&fixture)?;
    Ok(())
}

fn assert_cache_start_preserves_owner(fixture: &Fixture) -> TestResult {
    let state = fixture.state_root()?;
    let metadata = fs::read(state.join("services.json"))?;
    let key = fs::read(state.join("keys/cache.pub"))?;
    let snapshot = fs::read(fixture.root.path().join("runtime.json"))?;
    let output = fixture.run("start")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("explicit recovery")
            || output.stderr.contains("explicit host-wide maintenance")
            || output.stderr.contains("service migrate-legacy"),
        "{}",
        output.stderr
    );
    assert!(!fixture.root.path().join("run.argv").exists());
    for name in ["cache.lock", "publish-roots.json", "pending"] {
        assert!(!state.join(name).exists(), "{name}");
    }
    let log_path = fixture.root.path().join("runtime.log");
    if log_path.exists() {
        let log = fs::read_to_string(log_path)?;
        assert!(
            !log.lines()
                .any(|line| line.starts_with("rm ") || line.starts_with("image "))
        );
    }
    assert_eq!(fs::read(state.join("services.json"))?, metadata);
    assert_eq!(fs::read(state.join("keys/cache.pub"))?, key);
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        snapshot
    );
    Ok(())
}

#[test]
fn cache_start_preserves_guarded_owner_when_other_required_features_are_incompatible() -> TestResult
{
    let mut fixture = Fixture::new()?;
    fixture.snapshot[0]["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.dolt.auth");
    fixture.write_snapshot()?;
    assert_cache_start_preserves_owner(&fixture)?;
    Ok(())
}

#[test]
fn cache_start_does_not_treat_unknown_apple_state_as_stopped() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fixture.snapshot[0]["configuration"]["labels"]["wrix.cache.backend"] = json!("celler-v1");
    fixture.snapshot[0]["status"]["state"] = json!("stopping");
    fixture.write_snapshot()?;
    let state = fixture.state_root()?;
    let metadata = fs::read(state.join("services.json"))?;
    let snapshot = fs::read(fixture.root.path().join("runtime.json"))?;
    let output = fixture.run("start")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("invalid Apple container JSON"),
        "{}",
        output.stderr
    );
    assert!(!fixture.root.path().join("run.argv").exists());
    let log = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert!(
        !log.lines()
            .any(|line| line.starts_with("rm ") || line.starts_with("image "))
    );
    assert_eq!(fs::read(state.join("services.json"))?, metadata);
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        snapshot
    );
    Ok(())
}

#[test]
fn cache_start_preserves_a_stopped_native_backend_without_package_provenance() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fixture.snapshot[0]["configuration"]["labels"]["wrix.cache.backend"] = json!("celler-v1");
    fixture.snapshot[0]["status"]["state"] = json!("stopped");
    fixture.write_snapshot()?;
    assert_cache_start_preserves_owner(&fixture)?;
    Ok(())
}

#[test]
fn cache_start_rejects_foreign_running_unguarded_owner_before_layout_changes() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fixture.snapshot[0]["configuration"]["id"] = json!("wrix-service-peer");
    fixture.snapshot[0]["configuration"]["labels"]["wrix.workspace.hash"] =
        json!("0123456789abcdef");
    fixture.snapshot[0]["configuration"]["labels"]["wrix.workspace"] = json!("/peer");
    fixture.snapshot[0]["configuration"]["labels"]["wrix.cache.backend"] = json!("celler-v1");
    fixture.write_snapshot()?;
    assert_cache_start_preserves_owner(&fixture)?;
    Ok(())
}

#[test]
fn cache_start_rejects_competing_guarded_owner_without_claiming_shared_reuse() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fixture.snapshot[0]["configuration"]["id"] = json!("wrix-service-peer");
    fixture.snapshot[0]["configuration"]["labels"]["wrix.workspace.hash"] =
        json!("0123456789abcdef");
    fixture.snapshot[0]["configuration"]["labels"]["wrix.workspace"] = json!("/peer");
    fixture.write_snapshot()?;
    assert_cache_start_preserves_owner(&fixture)?;
    Ok(())
}

#[test]
fn cache_start_rejects_running_celler_in_the_other_runtime() -> TestResult {
    for backend in ["celler-v1", "celler-v2", "unknown-version"] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.path().join("runtime.json"), "[]")?;
        peer_runtime::install(
            &fixture.root.path().join("bin"),
            "wrix-service-peer",
            backend,
            "true",
        )?;
        assert_cache_start_preserves_owner(&fixture)?;
        let log = fs::read_to_string(fixture.root.path().join("bin/peer.log"))?;
        assert!(log.contains("{{.State.Running}}"));
        assert!(!log.lines().any(|line| line.starts_with("rm ")));
    }
    Ok(())
}

#[test]
fn identical_container_names_in_different_runtimes_do_not_authorize_reuse() -> TestResult {
    let fixture = Fixture::new()?;
    peer_runtime::install(
        &fixture.root.path().join("bin"),
        fixture.metadata["container_name"].as_str().unwrap(),
        "celler-v2",
        "true",
    )?;
    assert_cache_start_preserves_owner(&fixture)?;
    Ok(())
}

#[test]
fn inaccessible_other_runtime_does_not_authorize_cache_start() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.path().join("runtime.json"), "[]")?;
    peer_runtime::install(
        &fixture.root.path().join("bin"),
        "wrix-service-peer",
        "celler-v2",
        "unavailable",
    )?;
    let output = fixture.run("start")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("cannot connect to Podman machine"),
        "{}",
        output.stderr
    );
    assert!(!fixture.root.path().join("run.argv").exists());
    assert!(!fixture.state_root()?.join("cache.lock").exists());
    assert!(!fixture.root.path().join("runtime.log").exists());
    Ok(())
}

#[test]
fn podman_start_checks_apple_backend_ownership_before_selected_runtime_access() -> TestResult {
    let fixture = Fixture::new()?;
    let selected = fixture.root.path().join("bin/podman");
    fs::write(
        &selected,
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf 'selected runtime was accessed\\n' >&2\nexit 64\n",
    )?;
    set_mode(&selected, 0o755)?;
    let output = run_command(
        fixture
            .command()?
            .env("WRIX_CONTAINER_RUNTIME", selected)
            .args(["service", "start"]),
    )?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("service migrate-legacy"),
        "{}",
        output.stderr
    );
    assert!(!output.stderr.contains("selected runtime was accessed"));
    assert!(!fixture.root.path().join("run.argv").exists());
    assert!(!fixture.state_root()?.join("cache.lock").exists());
    Ok(())
}

#[test]
fn installed_other_runtime_with_missing_interpreter_is_not_treated_as_absent() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.path().join("runtime.json"), "[]")?;
    let peer = fixture.root.path().join("bin/podman");
    fs::write(&peer, "#!/definitely/missing/interpreter\n")?;
    set_mode(&peer, 0o755)?;
    let fallback = fixture.root.path().join("fallback");
    fs::create_dir(&fallback)?;
    peer_runtime::install_empty(&fallback)?;
    let mut command = fixture.command()?;
    let path = command
        .get_envs()
        .find_map(|(name, value)| (name == "PATH").then_some(value).flatten())
        .ok_or("missing fixture PATH")?;
    let mut paths: Vec<_> = std::env::split_paths(path).collect();
    paths.insert(1, fallback);
    command.env("PATH", std::env::join_paths(paths)?);
    fixture.api()?;
    let output = run_command(command.args(["service", "start"]))?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("service lifecycle I/O failed"),
        "{}",
        output.stderr
    );
    assert!(!fixture.root.path().join("runtime.log").exists());
    assert!(!fixture.root.path().join("run.argv").exists());
    assert!(!fixture.state_root()?.join("cache.lock").exists());
    Ok(())
}

#[test]
fn unknown_other_runtime_state_is_not_treated_as_stopped() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.path().join("runtime.json"), "[]")?;
    peer_runtime::install(
        &fixture.root.path().join("bin"),
        "wrix-service-peer",
        "celler-v2",
        "stopping",
    )?;
    let output = fixture.run("start")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("unknown running state"),
        "{}",
        output.stderr
    );
    assert!(!fixture.root.path().join("run.argv").exists());
    assert!(!fixture.state_root()?.join("cache.lock").exists());
    Ok(())
}

#[test]
fn stopped_backend_in_the_other_runtime_does_not_prevent_cache_reentry() -> TestResult {
    let fixture = Fixture::shared_cache()?;
    peer_runtime::install(
        &fixture.root.path().join("bin"),
        "wrix-service-peer",
        "celler-v1",
        "false",
    )?;
    let selected = fs::read(owner_selection(&fixture)?)?;
    let inventory = fs::read(fixture.root.path().join("runtime.json"))?;
    let output = fixture.run("start")?;
    assert!(output.status.success(), "{}", output.stderr);
    assert_eq!(fs::read(owner_selection(&fixture)?)?, selected);
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        inventory
    );
    let calls = fs::read_to_string(fixture.root.path().join("bin/peer.log"))?;
    assert!(!calls.lines().any(|line| line.starts_with("rm ")
        || line.starts_with("run ")
        || line.starts_with("start ")));
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn apple_cache_service_preserves_loopback_publication_and_narrow_managed_mounts() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fs::write(fixture.root.path().join("runtime.json"), "[]")?;
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
    let native: Value = serde_json::from_slice(&fs::read(
        fixture
            .root
            .path()
            .join("state/wrix/control/authority/native-plan.json"),
    )?)?;
    assert_eq!(
        publications,
        [
            format!("127.0.0.1:{}:8080", native["backend"]["cache"]),
            format!("127.0.0.1:{}:3306", native["backend"]["sql"]),
            format!("127.0.0.1:{}:4226", native["backend"]["compiler"]),
            format!("127.0.0.1:{}:3307", native["backend"]["sync"]),
        ]
    );
    let layout = wrix_cache::backend::layout::Layout::resolve(
        if cfg!(target_os = "macos") {
            wrix_cache::backend::layout::Platform::Darwin
        } else {
            wrix_cache::backend::layout::Platform::Linux
        },
        &fixture.root.path().join("home"),
        Some(&fixture.root.path().join("state")),
        Some(&fixture.root.path().join("cache")),
    )?;
    for mount in layout.mounts() {
        assert!(
            args.lines()
                .any(|arg| arg == format!("{}:{}:rw", mount.source.display(), mount.destination))
        );
    }
    assert!(!args.contains(":/cache:ro"));
    assert!(args.lines().any(|arg| arg == "wrix-shared-service"));
    assert!(args.lines().any(|arg| arg == "wrix.dolt.shared=native-v1"));
    let data = fixture.root.path().join("state/wrix/service-data/dolt");
    assert!(
        args.lines()
            .any(|arg| arg == format!("{}:{}:rw", data.display(), data.display()))
    );
    assert!(!args.contains("host-control"));
    assert!(!args.contains("/var/lib/wrix/beads/dolt"));
    Ok(())
}

#[test]
#[ignore = "requires host capability; run in the disposable native-system guest"]
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
fn host_lifecycle_lock_blocks_starts_from_unrelated_workspaces() -> TestResult {
    let fixture = Fixture::shared_cache()?;
    let peer = fixture.root.path().join("peer");
    fs::create_dir(&peer)?;
    initialize_repository(&peer)?;
    let lock = hold_host_lifecycle_lock(&fixture)?;
    fixture.api()?;
    let mut children = Vec::new();
    for workspace in [&fixture.workspace, &peer] {
        children.push(
            fixture
                .command()?
                .current_dir(workspace)
                .args(["service", "start"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()?,
        );
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        for child in &mut children {
            assert!(child.try_wait()?.is_none(), "startup bypassed host lock");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!fixture.root.path().join("runtime.log").exists());
    assert!(!fixture.state_root()?.join("publish-roots.json").exists());
    drop(lock);
    let mut children = children.into_iter();
    let selected = children.next().unwrap().wait_with_output()?;
    assert!(selected.status.success(), "{:?}", selected);
    let peer_output = children.next().unwrap().wait_with_output()?;
    if peer_output.status.success() {
        let output = run_command(
            fixture
                .command()?
                .current_dir(&peer)
                .args(["service", "endpoints"]),
        )?;
        assert!(output.status.success(), "{}", output.stderr);
        let metadata: Value = serde_json::from_str(&output.stdout)?;
        assert_eq!(
            metadata["endpoints"]["cache_http"],
            fixture.metadata["endpoints"]["cache_http"]
        );
        let state: PathBuf = serde_json::from_value(metadata["state_root"].clone())?;
        assert!(state.join("celler-publisher.json").is_file());
    } else {
        let diagnostic = String::from_utf8(peer_output.stderr)?;
        assert!(
            diagnostic.contains("explicit host-wide maintenance"),
            "{diagnostic}"
        );
    }
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn unsafe_host_lifecycle_lock_fails_before_owner_inspection_or_layout() -> TestResult {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new()?;
    let lock = hold_host_lifecycle_lock(&fixture)?;
    let host = fixture
        .state_root()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("host-lifecycle");
    drop(lock);
    fs::remove_file(host.join("lifecycle.lock"))?;
    let target = fixture.root.path().join("unrelated");
    fs::write(&target, "preserve")?;
    symlink(&target, host.join("lifecycle.lock"))?;
    for operation in ["start", "stop"] {
        let output = fixture.run(operation)?;
        assert!(!output.status.success());
        assert!(
            output.stderr.contains("host lifecycle coordination"),
            "{}",
            output.stderr
        );
    }
    assert_eq!(fs::read_to_string(target)?, "preserve");
    assert!(!fixture.root.path().join("runtime.log").exists());
    assert!(!fixture.state_root()?.join("publish-roots.json").exists());
    Ok(())
}

#[test]
fn workspace_stop_preserves_running_celler_even_with_cache_opt_out() -> TestResult {
    for backend in ["celler-v1", "celler-v2", "celler-unknown"] {
        for enabled in [true, false] {
            let mut fixture = Fixture::new()?;
            fixture.snapshot[0]["configuration"]["labels"]["wrix.cache.backend"] = json!(backend);
            fixture.write_snapshot()?;
            let state = fixture.state_root()?;
            let host = state.parent().unwrap().parent().unwrap();
            let peer = host.join("service-data/celler/peer-state");
            fs::write(&peer, "common and peer membership must survive")?;
            let paths = [
                fixture.root.path().join("runtime.json"),
                state.join("services.json"),
                state.join("keys/cache.pub"),
                host.join("host-control/celler-issuer/issuer.pem"),
                host.join("service-data/celler/jwt.pub"),
                peer,
            ];
            let before = paths.iter().map(fs::read).collect::<Result<Vec<_>, _>>()?;
            let output = run_command(
                fixture
                    .command()?
                    .env("WRIX_NIX_CACHE_ENABLE", enabled.to_string())
                    .args(["service", "stop"]),
            )?;
            assert!(!output.status.success());
            assert!(
                output
                    .stderr
                    .contains("workspace stop cannot remove shared Celler"),
                "{}",
                output.stderr
            );
            let after = paths.iter().map(fs::read).collect::<Result<Vec<_>, _>>()?;
            assert_eq!(after, before);
            let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
            assert!(!calls.lines().any(|line| line.starts_with("rm ")));
            assert!(!fixture.root.path().join("run.argv").exists());
        }
    }
    Ok(())
}

#[test]
fn cache_only_retirement_preserves_backend_and_reopens_only_after_registration() -> TestResult {
    let fixture = Fixture::shared_cache()?;
    let output = fixture.run("start")?;
    assert!(output.status.success(), "{}", output.stderr);
    let state = fixture.state_root()?;
    let identity = wrix_core::path::WorkspaceHash::parse(
        fixture.metadata["workspace_hash"].as_str().unwrap(),
    )?;
    let paths = [
        fixture.root.path().join("runtime.json"),
        state.join("services.json"),
        state.join("keys/cache.pub"),
        state.join("celler-publisher.json"),
    ];
    let before = paths.iter().map(fs::read).collect::<Result<Vec<_>, _>>()?;
    for _ in 0..2 {
        let output = fixture.run("stop")?;
        assert!(output.status.success(), "{}", output.stderr);
        assert!(output.stdout.contains("Stopped"), "{}", output.stdout);
    }
    assert!(matches!(
        wrix_cache::backend::publication::Publisher::load(&state, &identity),
        Err(wrix_cache::backend::publication::Error::Retired)
    ));
    let output = fixture.run("status")?;
    assert!(output.status.success(), "{}", output.stderr);
    assert!(output.stdout.contains("Stopped"), "{}", output.stdout);
    assert_eq!(
        paths.iter().map(fs::read).collect::<Result<Vec<_>, _>>()?,
        before
    );
    let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert!(!calls.lines().any(|line| line.starts_with("rm ")));
    assert!(!fixture.root.path().join("run.argv").exists());
    fs::write(fixture.root.path().join("celler-status"), "403")?;
    let output = fixture.run("start")?;
    assert!(!output.status.success());
    assert!(matches!(
        wrix_cache::backend::publication::Publisher::load(&state, &identity),
        Err(wrix_cache::backend::publication::Error::Retired)
    ));
    assert_eq!(fs::read(state.join("keys/cache.pub"))?, before[2]);
    fs::remove_file(fixture.root.path().join("celler-status"))?;
    let output = fixture.run("start")?;
    assert!(output.status.success(), "{}", output.stderr);
    assert!(wrix_cache::backend::publication::Publisher::load(&state, &identity).is_ok());
    assert!(!state.join("celler-retired").exists());
    assert_eq!(fs::read(state.join("keys/cache.pub"))?, before[2]);
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn cache_namespaces_reuse_the_selected_shared_owner_and_retire_without_removing_it() -> TestResult {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::shared_cache()?;
    let state = fixture.state_root()?;
    let host = state.parent().unwrap().parent().unwrap();
    let selection = host.join("host-lifecycle/celler-owner.json");
    let configuration = host.join("service-data/celler/server.toml");
    let selection_before = fs::read(&selection)?;
    let configuration_inode = fs::metadata(&configuration)?.ino();
    let owner_key = fs::read(state.join("keys/cache.pub"))?;
    let peer = fixture.root.path().join("peer");
    fs::create_dir(&peer)?;
    initialize_repository(&peer)?;
    let output = run_command(
        fixture
            .command()?
            .current_dir(&peer)
            .args(["service", "start"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let output = run_command(
        fixture
            .command()?
            .current_dir(&peer)
            .args(["service", "endpoints"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let metadata: Value = serde_json::from_str(&output.stdout)?;
    assert_eq!(
        metadata["endpoints"]["cache_http"],
        fixture.metadata["endpoints"]["cache_http"]
    );
    let peer_state: PathBuf = serde_json::from_value(metadata["state_root"].clone())?;
    let peer_identity =
        wrix_core::path::WorkspaceHash::parse(metadata["workspace_hash"].as_str().unwrap())?;
    assert!(wrix_cache::backend::publication::Publisher::load(&peer_state, &peer_identity).is_ok());
    let output = run_command(fixture.command()?.current_dir(&peer).args([
        "service",
        "endpoints",
        "--sandbox-cache",
    ]))?;
    assert!(!output.status.success());
    assert!(
        output
            .stderr
            .contains("no verified stable Apple guest data route"),
        "{}",
        output.stderr
    );
    let owner_identity = wrix_core::path::WorkspaceHash::parse(
        fixture.metadata["workspace_hash"].as_str().unwrap(),
    )?;
    for _ in 0..2 {
        let output = fixture.run("stop")?;
        assert!(output.status.success(), "{}", output.stderr);
        assert!(
            wrix_cache::backend::publication::Publisher::load(&peer_state, &peer_identity).is_ok()
        );
        assert!(matches!(
            wrix_cache::backend::publication::Publisher::load(&state, &owner_identity),
            Err(wrix_cache::backend::publication::Error::Retired)
        ));
        let output = run_command(
            fixture
                .command()?
                .current_dir(&peer)
                .args(["service", "status"]),
        )?;
        assert!(output.status.success(), "{}", output.stderr);
        assert!(output.stdout.contains("Running"), "{}", output.stdout);
        let output = fixture.run("start")?;
        assert!(output.status.success(), "{}", output.stderr);
        assert!(wrix_cache::backend::publication::Publisher::load(&state, &owner_identity).is_ok());
    }
    let output = run_command(
        fixture
            .command()?
            .current_dir(&peer)
            .args(["service", "stop"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    assert!(matches!(
        wrix_cache::backend::publication::Publisher::load(&peer_state, &peer_identity),
        Err(wrix_cache::backend::publication::Error::Retired)
    ));
    assert_eq!(fs::read(&selection)?, selection_before);
    assert_eq!(fs::metadata(configuration)?.ino(), configuration_inode);
    assert_eq!(fs::read(state.join("keys/cache.pub"))?, owner_key);
    let remaining: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("runtime.json"))?)?;
    assert_eq!(remaining, fixture.snapshot);
    let log = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert!(
        !log.lines()
            .any(|line| line.starts_with("run ") || line.starts_with("rm "))
    );
    // Native SQL peer retirement and authenticated writes use genuine Beads
    // databases in public_cli_shared_native_backend_preserves_namespaces_and_denies_peer_sql.
    Ok(())
}

fn selected_inactive_owner(stopped: bool) -> TestResult<Fixture> {
    let mut fixture = Fixture::shared_cache()?;
    let output = fixture.run("start")?;
    assert!(output.status.success(), "{}", output.stderr);
    fixture.server.borrow_mut().take();
    if stopped {
        fixture.snapshot[0]["status"]["state"] = json!("stopped");
        fixture.write_snapshot()?;
    } else {
        fs::write(fixture.root.path().join("runtime.json"), "[]")?;
    }
    Ok(fixture)
}

fn owner_selection(fixture: &Fixture) -> TestResult<PathBuf> {
    let state = fixture.state_root()?;
    Ok(state
        .parent()
        .and_then(Path::parent)
        .ok_or("missing host state root")?
        .join("host-lifecycle/celler-owner.json"))
}

#[test]
fn recovery_recreates_only_absent_or_stopped_selected_owner_and_preserves_data() -> TestResult {
    for stopped in [false, true] {
        let fixture = selected_inactive_owner(stopped)?;
        let selection = owner_selection(&fixture)?;
        let previous: Value = serde_json::from_slice(&fs::read(&selection)?)?;
        let host = selection.parent().unwrap().parent().unwrap();
        let persistent = host.join("service-data/celler/persistent-test");
        fs::write(&persistent, "retain data")?;
        let issuer = host.join("host-control/celler-issuer/issuer.pem");
        let authority = fs::read(&issuer)?;
        let ordinary = fixture.run("start")?;
        assert!(!ordinary.status.success());
        assert!(
            ordinary
                .stderr
                .contains(if stopped { "Stopped" } else { "Missing" }),
            "{}",
            ordinary.stderr
        );
        assert!(ordinary.stderr.contains("run `wrix service recover`"));
        let output = fixture.run("recover")?;
        assert!(output.status.success(), "{}", output.stderr);
        assert_eq!(fs::read_to_string(&persistent)?, "retain data");
        assert_eq!(fs::read(&issuer)?, authority);
        let recovered: Value = serde_json::from_slice(&fs::read(&selection)?)?;
        for field in ["runtime", "container_name", "cache_http_port"] {
            assert_eq!(previous[field], recovered[field], "{field}");
        }
        assert_eq!(recovered["schema_version"], 3);
        assert!(recovered.get("workspace_hash").is_none());
        let args = fs::read_to_string(fixture.root.path().join("run.argv"))?;
        assert!(args.contains("wrix.service.scope=host"));
        assert!(!args.contains("/var/lib/wrix/beads"));
        assert!(
            !args
                .lines()
                .any(|argument| argument.ends_with(":/run/wrix:rw"))
        );
        assert!(fixture.root.path().join("run.argv").exists());
        let output = fixture.run("start")?;
        assert!(output.status.success(), "{}", output.stderr);
    }
    Ok(())
}

#[test]
fn missing_historical_owner_can_be_renamed_then_recovered_without_resetting_authority() -> TestResult
{
    let fixture = selected_inactive_owner(false)?;
    let selection = owner_selection(&fixture)?;
    let mut previous: Value = serde_json::from_slice(&fs::read(&selection)?)?;
    previous["container_name"] = json!("loom-historical-service");
    fs::write(&selection, serde_json::to_vec(&previous)?)?;
    let host = selection.parent().unwrap().parent().unwrap();
    let key = host.join("host-control/celler-issuer/issuer.pem");
    let authority = fs::read(&key)?;
    let persistent = host.join("service-data/celler/retained-rename-test");
    fs::write(&persistent, b"persistent history")?;
    let output = run_command(
        fixture
            .command()?
            .args(["service", "rename", "wrix-host-service"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    assert!(output.stdout.contains("renamed to wrix-host-service"));
    assert!(!fixture.root.path().join("run.argv").exists());
    let renamed: Value = serde_json::from_slice(&fs::read(&selection)?)?;
    previous["container_name"] = json!("wrix-host-service");
    assert_eq!(renamed, previous);
    let output = fixture.run("recover")?;
    assert!(output.status.success(), "{}", output.stderr);
    assert_eq!(fs::read(&key)?, authority);
    assert_eq!(fs::read(&persistent)?, b"persistent history");
    let args = fs::read_to_string(fixture.root.path().join("run.argv"))?;
    assert!(args.contains("wrix-host-service"));
    Ok(())
}

#[test]
fn invalid_rename_arguments_fail_before_host_state_or_runtime_access() -> TestResult {
    let root = tempfile::tempdir()?;
    for arguments in [
        vec!["service", "rename"],
        vec!["service", "rename", "../escape"],
        vec!["service", "rename", "--host"],
        vec!["service", "rename", "valid", "extra"],
    ] {
        let output = host_wrix_command_with_path(root.path(), &[])?
            .args(arguments)
            .env("HOME", root.path().join("home"))
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("WRIX_CONTAINER_RUNTIME", "/missing/runtime")
            .output()?;
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("rename <name>")
                || String::from_utf8_lossy(&output.stderr)
                    .contains("invalid service container name")
                || String::from_utf8_lossy(&output.stderr)
                    .contains("--host is supported only for service information and stop"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(fs::read_dir(root.path())?.next().is_none());
    }
    Ok(())
}

#[test]
fn running_owner_rename_is_refused_before_any_ownership_change() -> TestResult {
    let fixture = Fixture::shared_cache()?;
    let selection = owner_selection(&fixture)?;
    let before = fs::read(&selection)?;
    let output = run_command(
        fixture
            .command()?
            .args(["service", "rename", "renamed-service"]),
    )?;
    assert!(!output.status.success());
    assert!(output.stderr.contains("is running"), "{}", output.stderr);
    assert_eq!(fs::read(&selection)?, before);
    assert!(!selection.parent().unwrap().join("owner-rename").exists());
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn repeated_recovery_reports_foreign_workspace_and_migration_without_mutation() -> TestResult {
    let mut fixture = Fixture::shared_cache()?;
    let other = fixture.root.path().join("other-repository");
    fs::create_dir(&other)?;
    let mut legacy = fixture.snapshot[0].clone();
    legacy["configuration"]["id"] = json!("foreign-legacy");
    legacy["configuration"]["labels"]["wrix.workspace"] = json!(other);
    legacy["configuration"]["labels"]["wrix.workspace.hash"] = json!("e".repeat(64));
    legacy["configuration"]["labels"]["wrix.service.supervision"] = json!("1");
    legacy["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.dolt.shared");
    legacy["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.cache.backend");
    fixture.snapshot.as_array_mut().unwrap().push(legacy);
    fixture.write_snapshot()?;
    let before = fs::read(fixture.root.path().join("runtime.json"))?;
    let state = fixture.state_root()?;
    let secret = fs::read(state.join("keys/cache.secret"))?;
    let public = fs::read(state.join("keys/cache.pub"))?;
    for _ in 0..2 {
        for command in ["start", "recover"] {
            let output = fixture.run(command)?;
            assert!(!output.status.success());
            for expected in [
                "foreign-legacy",
                "other-repository",
                "container",
                "service migrate-legacy",
                "stale shell/package",
            ] {
                assert!(output.stderr.contains(expected), "{}", output.stderr);
            }
            assert!(
                !output.stderr.contains("service recover"),
                "{}",
                output.stderr
            );
        }
    }
    let mut command = fixture.command()?;
    let output = run_command(command.args(["service", "migrate-legacy", "--prepare"]))?;
    assert!(output.status.success(), "{}", output.stderr);
    assert!(
        output.stdout.contains("foreign-legacy") && output.stdout.contains("maintenance approval")
    );
    let mut command = fixture.command()?;
    let output = run_command(command.args(["service", "migrate-legacy", "--quiesced"]))?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("must be gracefully stopped"),
        "{}",
        output.stderr
    );
    assert_eq!(fs::read(fixture.root.path().join("runtime.json"))?, before);
    assert_eq!(fs::read(state.join("keys/cache.secret"))?, secret);
    assert_eq!(fs::read(state.join("keys/cache.pub"))?, public);
    let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert!(!calls.lines().any(|line| line.starts_with("stop ")
        || line.starts_with("rm ")
        || line.starts_with("run ")));
    Ok(())
}

#[test]
fn unrelated_stopped_legacy_service_does_not_block_shared_owner_reentry() -> TestResult {
    let fixture = Fixture::shared_cache()?;
    let mut snapshot: Vec<Value> =
        serde_json::from_slice(&fs::read(fixture.root.path().join("runtime.json"))?)?;
    let mut peer = snapshot[0].clone();
    peer["configuration"]["id"] = json!("openwrt-service");
    peer["status"]["state"] = json!("stopped");
    assert!(
        peer["configuration"]["labels"]
            .as_object_mut()
            .ok_or("missing labels")?
            .remove("wrix.dolt.shared")
            .is_some()
    );
    snapshot.push(peer.clone());
    fs::write(
        fixture.root.path().join("runtime.json"),
        serde_json::to_vec(&snapshot)?,
    )?;
    let output = fixture.run("start")?;
    assert!(output.status.success(), "{}", output.stderr);
    let after: Vec<Value> =
        serde_json::from_slice(&fs::read(fixture.root.path().join("runtime.json"))?)?;
    assert!(after.iter().any(|entry| entry == &peer));
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn unrelated_stopped_legacy_service_does_not_block_fresh_shared_bootstrap() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    let mut peer = fixture.snapshot[0].clone();
    peer["configuration"]["id"] = json!("openwrt-service");
    peer["configuration"]["labels"]["wrix.workspace.hash"] = json!("b".repeat(64));
    peer["status"]["state"] = json!("stopped");
    fixture.snapshot = json!([peer.clone()]);
    fixture.write_snapshot()?;
    let output = fixture.run("start")?;
    assert!(output.status.success(), "{}", output.stderr);
    let after: Vec<Value> =
        serde_json::from_slice(&fs::read(fixture.root.path().join("runtime.json"))?)?;
    assert!(after.iter().any(|entry| entry == &peer));
    assert!(
        after
            .iter()
            .any(|entry| entry["configuration"]["id"] == "wrix-host-service"
                && entry["status"]["state"] == "running")
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn repair_upgrades_cached_controller_and_recovers_backend_with_native_authenticated_readiness()
-> TestResult {
    let fixture = Fixture::shared_cache()?;
    // The old controller established its frontend endpoints while its backend
    // was live; only afterward did that backend disappear, as on the host.
    fixture
        .controller
        .borrow_mut()
        .as_mut()
        .ok_or("missing owned controller")?
        .stop()?;
    let manager = maintenance::Maintenance::start(fixture.root.path())?;
    fixture.server.borrow_mut().take();
    fs::write(fixture.root.path().join("runtime.json"), b"[]")?;
    let registry = manager.control.join("authority/namespaces.json");
    let before: Value = serde_json::from_slice(&fs::read(&registry)?)?;
    assert_eq!(before["version"], 1);
    let management = manager.control.join("authority/management.key");
    let management_key = fs::read(&management)?;
    let host = manager.control.parent().unwrap();
    let issuer = host.join("host-control/celler-issuer/issuer.pem");
    let issuer_key = fs::read(&issuer)?;
    let mut command = fixture.command()?;
    manager.configure(&mut command);
    let output = run_command(command.args(["service", "recover"]))?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("service repair"),
        "{}",
        output.stderr
    );
    assert!(!fixture.root.path().join("run.argv").exists());
    fixture.api()?;
    let mut command = fixture.command()?;
    manager.configure(&mut command);
    let output = run_command(command.args(["service", "repair"]))?;
    assert!(
        output.status.success(),
        "{}; controller: {}; supervisor: {}",
        output.stderr,
        fs::read_to_string(manager.control.join("new-controller.stderr"))?,
        fs::read_to_string(manager.control.join("manager.calls"))?
    );
    assert!(output.stdout.contains("runtime: Running"));
    let after: Value = serde_json::from_slice(&fs::read(&registry)?)?;
    assert_eq!(after["version"], 2);
    assert_eq!(after["entries"], before["entries"]);
    assert_eq!(fs::read(&management)?, management_key);
    assert_eq!(fs::read(&issuer)?, issuer_key);
    assert_eq!(
        wrix_service::control::supervision::Package::load(&manager.control.join("package.json"))?,
        manager.package
    );
    let mut command = fixture.command()?;
    manager.configure(&mut command);
    let output = run_command(command.args(["service", "recover"]))?;
    assert!(output.status.success(), "{}", output.stderr);
    Ok(())
}

#[test]
fn recovery_checks_legacy_controller_before_starting_or_creating_backend() -> TestResult {
    let fixture = selected_inactive_owner(false)?;
    fixture.controller.borrow_mut().take();
    let selection = owner_selection(&fixture)?;
    let before = fs::read(&selection)?;
    let host = selection.parent().unwrap().parent().unwrap();
    let authority = host.join("control/authority");
    wrix_core::private_directory::prepare_with_parents(&authority)?;
    let policy = serde_json::to_vec(&json!({"version": 1, "entries": []}))?;
    fs::write(authority.join("namespaces.json"), &policy)?;
    set_mode(&authority.join("namespaces.json"), 0o600)?;
    let fence = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(authority.join("ownership"))?;
    set_mode(&authority.join("ownership"), 0o600)?;
    fence.lock_exclusive()?;
    let output = fixture.run("recover")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("wrix service repair"),
        "{}",
        output.stderr
    );
    assert!(!fixture.root.path().join("run.argv").exists());
    assert_eq!(fs::read(&selection)?, before);
    assert_eq!(fs::read(authority.join("namespaces.json"))?, policy);
    assert!(
        !host
            .join("host-lifecycle/celler-candidate/celler-owner.json")
            .exists()
    );
    Ok(())
}

#[test]
fn recovery_guidance_uses_short_command_regardless_of_selected_wrapper() -> TestResult {
    let fixture = selected_inactive_owner(false)?;
    let wrapper = fixture.root.path().join("selected Wrix's wrapper");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nset -eu\nexport WRIX_INVOKED_BIN=\"$0\"\nexport WRIX_SERVICE_PACKAGE_BINDING='{}'\nexec '{}' \"$@\"\n",
            fixture.root.path().join("binding.json").display(),
            env!("CARGO_BIN_EXE_wrix"),
        ),
    )?;
    set_mode(&wrapper, 0o755)?;
    let global = fixture.root.path().join("bin/wrix");
    fs::write(
        &global,
        "#!/bin/sh\nprintf '%s\\n' 'unknown service command: recover' >&2\nexit 2\n",
    )?;
    set_mode(&global, 0o755)?;
    let before = fs::read(owner_selection(&fixture)?)?;
    let mut entry = fixture.command()?;
    entry
        .env("WRIX_INVOKED_BIN", &wrapper)
        .env("WRIX_BIN", &global)
        .args(["service", "start"]);
    let output = run_command(&mut entry)?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("run `wrix service recover`"),
        "{}",
        output.stderr
    );
    assert!(
        !output
            .stderr
            .contains(fixture.workspace.canonicalize()?.to_str().unwrap()),
        "{}",
        output.stderr
    );
    assert_eq!(fs::read(owner_selection(&fixture)?)?, before);
    assert!(!fixture.root.path().join("run.argv").exists());
    let command = output
        .stderr
        .split('`')
        .nth(1)
        .ok_or("missing recovery command")?;
    assert_eq!(command, "wrix service recover");
    Ok(())
}

#[test]
fn recovery_rejects_competing_owners_in_either_runtime_without_mutation() -> TestResult {
    for other_runtime in [false, true] {
        let mut fixture = selected_inactive_owner(false)?;
        let selection = owner_selection(&fixture)?;
        let before = fs::read(&selection)?;
        if other_runtime {
            peer_runtime::install(
                &fixture.root.path().join("bin"),
                "peer",
                "celler-v2",
                "true",
            )?;
        } else {
            fixture.snapshot[0]["configuration"]["id"] = json!("competing-service");
            fixture.write_snapshot()?;
        }
        let inventory = fs::read(fixture.root.path().join("runtime.json"))?;
        let output = fixture.run("recover")?;
        assert!(!output.status.success());
        assert!(
            output.stderr.contains("foreign legacy service"),
            "{}",
            output.stderr
        );
        assert!(
            output.stderr.contains("service migrate-legacy"),
            "{}",
            output.stderr
        );
        assert_eq!(fs::read(&selection)?, before);
        assert_eq!(
            fs::read(fixture.root.path().join("runtime.json"))?,
            inventory
        );
        assert!(!fixture.root.path().join("run.argv").exists());
    }
    Ok(())
}

#[test]
fn unavailable_owner_guidance_does_not_require_a_foreign_workspace() -> TestResult {
    let fixture = selected_inactive_owner(false)?;
    let other = fixture.root.path().join("other");
    fs::create_dir(&other)?;
    common::run_git(&other, &["init", "-q"])?;
    let selection = owner_selection(&fixture)?;
    let before = fs::read(&selection)?;
    let output = run_command(
        fixture
            .command()?
            .current_dir(&other)
            .args(["service", "start"]),
    )?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("run `wrix service recover`"),
        "{}",
        output.stderr
    );
    assert_eq!(fs::read(&selection)?, before);
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn recovery_is_host_scoped_without_the_original_repository_or_caller_cache_policy() -> TestResult {
    let fixture = selected_inactive_owner(false)?;
    let other = fixture.root.path().join("other");
    fs::create_dir(&other)?;
    fs::write(other.join("wrix.toml"), "this is not valid TOML [")?;
    fixture.api()?;
    fs::remove_dir_all(&fixture.workspace)?;
    let selection = owner_selection(&fixture)?;
    let before: Value = serde_json::from_slice(&fs::read(&selection)?)?;
    let output = run_command(
        fixture
            .command()?
            .current_dir(&other)
            .args(["service", "recover"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    let recovered: Value = serde_json::from_slice(&fs::read(&selection)?)?;
    assert_eq!(recovered["container_name"], before["container_name"]);
    assert_eq!(recovered["cache_http_port"], before["cache_http_port"]);
    assert_eq!(recovered["schema_version"], 3);
    assert!(recovered.get("workspace_hash").is_none());
    assert!(fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn recovery_finishes_interrupted_initial_creation_without_the_original_repository() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fs::write(fixture.root.path().join("runtime.json"), "[]")?;
    fs::write(fixture.root.path().join("celler-status"), "403")?;
    let output = fixture.run("start")?;
    assert!(!output.status.success());
    let selection = owner_selection(&fixture)?;
    assert!(!selection.exists());
    let candidate = selection
        .parent()
        .unwrap()
        .join("celler-candidate/celler-owner.json");
    assert!(candidate.exists());
    let outside = fixture.root.path().join("outside");
    fs::create_dir(&outside)?;
    fs::remove_dir_all(&fixture.workspace)?;
    fs::write(fixture.root.path().join("celler-status"), "200")?;
    let output = run_command(
        fixture
            .command()?
            .current_dir(&outside)
            .args(["service", "recover"]),
    )?;
    assert!(output.status.success(), "{}", output.stderr);
    assert!(!candidate.exists());
    let selected: Value = serde_json::from_slice(&fs::read(selection)?)?;
    assert_eq!(selected["container_name"], "wrix-host-service");
    assert!(selected.get("workspace_hash").is_none());
    let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("run -d --name wrix-host-service "))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn recovery_requires_existing_private_authority_before_removing_a_stopped_container() -> TestResult
{
    let fixture = selected_inactive_owner(true)?;
    let selection = owner_selection(&fixture)?;
    let before = fs::read(&selection)?;
    let host = selection.parent().unwrap().parent().unwrap();
    fs::remove_file(host.join("host-control/celler-issuer/issuer.pem"))?;
    let inventory = fs::read(fixture.root.path().join("runtime.json"))?;
    let output = fixture.run("recover")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("host-private Celler issuer"),
        "{}",
        output.stderr
    );
    assert_eq!(fs::read(&selection)?, before);
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        inventory
    );
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn recovery_refuses_unobservable_competing_runtime() -> TestResult {
    let fixture = selected_inactive_owner(false)?;
    peer_runtime::install(
        &fixture.root.path().join("bin"),
        "peer",
        "celler-v2",
        "unavailable",
    )?;
    let selection = owner_selection(&fixture)?;
    let before = fs::read(&selection)?;
    let output = fixture.run("recover")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("cannot connect"),
        "{}",
        output.stderr
    );
    assert_eq!(fs::read(&selection)?, before);
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn recovery_preserves_selection_when_authenticated_readiness_fails() -> TestResult {
    let fixture = selected_inactive_owner(false)?;
    let selection = owner_selection(&fixture)?;
    let before = fs::read(&selection)?;
    fs::write(fixture.root.path().join("celler-status"), "403")?;
    let output = fixture.run("recover")?;
    assert!(!output.status.success());
    assert!(output.stderr.contains("readiness"), "{}", output.stderr);
    assert_eq!(fs::read(&selection)?, before);
    assert!(fixture.root.path().join("run.argv").exists());
    let candidate = selection
        .parent()
        .unwrap()
        .join("celler-candidate/celler-owner.json");
    assert!(candidate.exists());
    fs::write(fixture.root.path().join("celler-status"), "200")?;
    let output = fixture.run("recover")?;
    assert!(output.status.success(), "{}", output.stderr);
    assert!(!candidate.exists());
    let recovered: Value = serde_json::from_slice(&fs::read(&selection)?)?;
    assert_eq!(recovered["schema_version"], 3);
    let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("run -d --name wrix-host-service "))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn recovery_refuses_incompatible_running_backend() -> TestResult {
    let mut fixture = selected_inactive_owner(true)?;
    fixture.snapshot[0]["status"]["state"] = json!("running");
    fixture.snapshot[0]["configuration"]["labels"]["wrix.cache.backend"] = json!("celler-v1");
    fixture.write_snapshot()?;
    let selection = owner_selection(&fixture)?;
    let before = fs::read(&selection)?;
    let inventory = fs::read(fixture.root.path().join("runtime.json"))?;
    let output = fixture.run("recover")?;
    assert!(!output.status.success());
    assert!(output.stderr.contains("incompatible"), "{}", output.stderr);
    assert_eq!(fs::read(&selection)?, before);
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        inventory
    );
    assert!(!fixture.root.path().join("run.argv").exists());
    Ok(())
}

#[test]
fn recovery_does_not_treat_stopped_alternate_backend_as_a_running_owner() -> TestResult {
    let fixture = selected_inactive_owner(false)?;
    peer_runtime::install(
        &fixture.root.path().join("bin"),
        "peer",
        "celler-v1",
        "false",
    )?;
    let output = fixture.run("recover")?;
    assert!(output.status.success(), "{}", output.stderr);
    Ok(())
}

#[test]
fn recovery_waits_for_host_lifecycle_exclusion_before_runtime_inspection() -> TestResult {
    let fixture = selected_inactive_owner(false)?;
    fixture.api()?;
    fs::write(fixture.root.path().join("runtime.log"), "")?;
    let lock = hold_host_lifecycle_lock(&fixture)?;
    let mut child = fixture
        .command()?
        .args(["service", "recover"])
        .stdout(std::process::Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        assert!(child.try_wait()?.is_none(), "recovery bypassed host lock");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read(fixture.root.path().join("runtime.log"))?, b"");
    drop(lock);
    assert!(child.wait()?.success());
    Ok(())
}

#[test]
fn concurrent_recovery_creates_one_owner() -> TestResult {
    let fixture = selected_inactive_owner(false)?;
    fixture.api()?;
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            fixture
                .command()?
                .args(["service", "recover"])
                .stdout(std::process::Stdio::null())
                .spawn()?,
        );
    }
    for mut child in children {
        assert!(child.wait()?.success());
    }
    let log = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("run -d --name wrix-host-service "))
            .count(),
        1,
        "{log}"
    );
    Ok(())
}

#[test]
fn selected_celler_owner_is_not_replaced_when_its_runtime_identity_is_unconfirmed() -> TestResult {
    for (change, value) in [
        ("/status/state", json!("stopped")),
        (
            "/configuration/labels/wrix.service.scope",
            json!("workspace"),
        ),
        ("/configuration/labels/wrix.cache.backend", json!("unknown")),
        ("/configuration/publishedPorts", json!([])),
    ] {
        let mut fixture = Fixture::shared_cache()?;
        *fixture.snapshot[0].pointer_mut(change).unwrap() = value;
        fixture.write_snapshot()?;
        let state = fixture.state_root()?;
        let host = state.parent().unwrap().parent().unwrap();
        let selection = host.join("host-lifecycle/celler-owner.json");
        let before = fs::read(&selection)?;
        let runtime = fs::read(fixture.root.path().join("runtime.json"))?;
        let output = fixture.run("start")?;
        assert!(!output.status.success(), "{change}");
        assert!(
            output.stderr.contains("run `wrix service recover`")
                || output.stderr.contains("explicit recovery"),
            "{}",
            output.stderr
        );
        assert_eq!(fs::read(&selection)?, before);
        assert_eq!(fs::read(fixture.root.path().join("runtime.json"))?, runtime);
        assert!(!fixture.root.path().join("run.argv").exists());
        let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
        assert!(!calls.lines().any(|line| line.starts_with("rm ")));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn legacy_cohosted_start_preserves_live_components_and_peer_inventory_pending_maintenance()
-> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.enable_components()?;
    let component = fixture.component.as_ref().unwrap();
    let celler_pid = fs::read(component.root().join("celler.pid"))?;
    let peer = fixture.snapshot[0].clone();
    let mut peer = peer;
    peer["configuration"]["id"] = json!("live-peer");
    peer["configuration"]["labels"]["wrix.workspace.hash"] = json!("0123456789abcdef");
    fixture.snapshot.as_array_mut().unwrap().push(peer);
    fixture.write_snapshot()?;
    let before = fs::read(fixture.root.path().join("runtime.json"))?;
    let source = fixture.workspace.join(".beads/dolt/retained");
    fs::write(&source, "legacy database evidence")?;
    assert_cache_start_preserves_owner(&fixture)?;
    assert_eq!(fs::read(fixture.root.path().join("runtime.json"))?, before);
    assert_eq!(fs::read(component.root().join("celler.pid"))?, celler_pid);
    assert_eq!(fs::read_to_string(source)?, "legacy database evidence");
    let pid = String::from_utf8(celler_pid)?;
    assert!(
        Command::new("kill")
            .args(["-0", pid.trim()])
            .status()?
            .success()
    );
    assert!(
        !fixture
            .state_root()?
            .join("component-transition.json")
            .exists()
    );
    // The shared replacement's actual native retirement/peer continuity is
    // exercised by public_cli_shared_native_backend_preserves_namespaces_and_denies_peer_sql.
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn unconfirmed_legacy_component_dispatch_fences_later_mutations_and_passive_status() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.enable_components()?;
    // Ordinary start must refuse legacy replacement; it is not prerequisite
    // authority for a namespace-scoped stop of the already-running supervisor.
    fixture.component.as_mut().unwrap().stop()?;
    let output = fixture.run("stop")?;
    assert!(!output.status.success());
    let state = fixture.state_root()?;
    let pending = fs::read(state.join("component-transition.json"))?;
    let log = fs::read(fixture.root.path().join("runtime.log"))?;
    for operation in ["start", "stop", "status"] {
        let output = fixture.run(operation)?;
        assert!(!output.status.success());
        assert!(
            output
                .stderr
                .contains("prior component dispatch is unconfirmed"),
            "{}",
            output.stderr
        );
        assert_eq!(fs::read(state.join("component-transition.json"))?, pending);
        assert_eq!(fs::read(fixture.root.path().join("runtime.log"))?, log);
    }
    Ok(())
}

#[test]
fn workspace_stop_rejects_uncertain_backend_state_before_removal() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.snapshot[0]["status"]["state"] = json!("stopping");
    fixture.write_snapshot()?;
    let before = fs::read(fixture.root.path().join("runtime.json"))?;
    let output = fixture.run("stop")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("invalid Apple container JSON"),
        "{}",
        output.stderr
    );
    let calls = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert!(!calls.lines().any(|line| line.starts_with("rm ")));
    assert_eq!(fs::read(fixture.root.path().join("runtime.json"))?, before);
    Ok(())
}

#[test]
fn workspace_stop_can_remove_running_legacy_static_service() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.snapshot[0]["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.cache.backend");
    fixture.write_snapshot()?;
    let output = fixture.run("stop")?;
    assert!(output.status.success(), "{}", output.stderr);
    let snapshot: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("runtime.json"))?)?;
    assert_eq!(snapshot, json!([]));
    Ok(())
}

#[test]
fn host_lifecycle_lock_blocks_stop_before_container_removal() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fixture.snapshot[0]["configuration"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("wrix.cache.backend");
    fixture.snapshot[0]["status"]["state"] = json!("stopped");
    fixture.write_snapshot()?;
    let snapshot = fs::read(fixture.root.path().join("runtime.json"))?;
    let lock = hold_host_lifecycle_lock(&fixture)?;
    let mut child = fixture
        .command()?
        .args(["service", "stop"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        assert!(child.try_wait()?.is_none(), "stop bypassed host lock");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!fixture.root.path().join("runtime.log").exists());
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        snapshot
    );
    drop(lock);
    let output = child.wait_with_output()?;
    assert!(output.status.success(), "{:?}", output);
    let log = fs::read_to_string(fixture.root.path().join("runtime.log"))?;
    assert_eq!(
        log.lines().filter(|line| line.starts_with("rm ")).count(),
        1
    );
    Ok(())
}

fn hold_host_lifecycle_lock(fixture: &Fixture) -> TestResult<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    let root = fixture
        .state_root()?
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| std::io::Error::other("fixture has no host state root"))?
        .join("host-lifecycle");
    wrix_core::private_directory::prepare_with_parents(&root)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(root.join("lifecycle.lock"))?;
    file.lock_exclusive()?;
    Ok(file)
}

#[test]
fn concurrent_service_starts_share_one_lifecycle_owner() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.disable_dolt()?;
    fs::write(fixture.root.path().join("runtime.json"), "[]")?;
    fixture.api()?;
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
        log.lines()
            .filter(|line| line.starts_with("run -d --name wrix-host-service "))
            .count(),
        1,
        "{log}"
    );
    Ok(())
}

#[test]
fn service_fixture_removes_ambient_image_metadata() -> TestResult {
    let fixture = Fixture::new()?;
    let command = fixture.command()?;
    for name in [
        "WRIX_SERVICE_IMAGE",
        "WRIX_SERVICE_IMAGE_SOURCE",
        "WRIX_SERVICE_IMAGE_SOURCE_KIND",
        "WRIX_SERVICE_IMAGE_DIGEST",
    ] {
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == name && value.is_none()),
            "fixture must remove inherited {name}"
        );
    }
    Ok(())
}

#[test]
fn managed_checkout_outside_devshell_skips_import_and_preserves_chained_hooks() -> TestResult {
    // The native fixture now performs this check on a genuinely seeded and
    // adopted shared namespace. Empty .beads/dolt directories cannot stand in
    // for a native database, nor can a fabricated version witness admit one.
    public_cli_shared_native_backend_preserves_namespaces_and_denies_peer_sql()
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
    let before = common::read_namespace_document(&fixture.state_root()?.join("services.json"))?;
    let output = fixture.run("start")?;
    assert!(!output.status.success());
    assert!(
        output.stderr.contains("Apple container JSON"),
        "{}",
        output.stderr
    );
    assert_eq!(
        common::read_namespace_document(&fixture.state_root()?.join("services.json"))?,
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
    let mut command = fixture.runtime_command()?;
    command.args(["run", "-d", "--name", name]);
    for (key, value) in fixture.snapshot[0]["configuration"]["labels"]
        .as_object()
        .unwrap()
    {
        command
            .arg("--label")
            .arg(format!("{key}={}", value.as_str().unwrap()));
    }
    for port in fixture.snapshot[0]["configuration"]["publishedPorts"]
        .as_array()
        .unwrap()
    {
        command.arg("-p").arg(format!(
            "127.0.0.1:{}:{}",
            port["hostPort"], port["containerPort"]
        ));
    }
    let image = format!("sha256:{}", "a".repeat(64));
    command.args([image.as_str(), "sh", "-c", "sleep infinity"]);
    assert!(command.status()?.success());
    let replacement: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("runtime.json"))?)?;
    let mut expected = fixture.snapshot.clone();
    expected[0]["configuration"]["image"] = json!({"descriptor": {"digest": image}});
    assert_eq!(replacement, expected);
    Ok(())
}
