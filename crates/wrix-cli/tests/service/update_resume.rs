//! Recovery consumes artifacts produced by real CLI attempts, not fabricated journals/receipts.
use super::*;
#[path = "controller_restart.rs"]
mod controller_restart;
use std::{os::unix::fs::MetadataExt, process::Stdio};

fn bind_target(fixture: &Fixture) -> TestResult<PathBuf> {
    let target = fixture.root.path().join("recorded-target");
    fs::create_dir(&target)?;
    let source = target.join("source");
    let digest = target.join("digest");
    fs::write(&source, b"retained image source")?;
    fs::write(&digest, format!("sha256:{}\n", "b".repeat(64)))?;
    let binding = service_binding::write(
        &target,
        &fixture.root.path().join("bin"),
        "docker-archive",
        &source,
        &digest,
    )?;
    let mut candidate: Value = serde_json::from_slice(&fs::read(&binding)?)?;
    candidate["image"] = json!("wrix-service:recorded-target");
    fs::write(&binding, serde_json::to_vec(&candidate)?)?;
    let nix = fixture.root.path().join("bin/nix-store");
    let original = fs::read_to_string(&nix)?;
    fs::write(
        nix,
        format!(
            "{}\n{original}",
            include_str!("../fixtures/update-image-realisation.sh")
        ),
    )?;
    Ok(binding)
}

fn target_command(fixture: &Fixture, binding: &Path) -> TestResult<Command> {
    let mut command = fixture.command()?;
    command
        .env("WRIX_SERVICE_PACKAGE_BINDING", binding)
        .env("WRIX_TEST_DIGEST", "b".repeat(64))
        .env("WRIX_TEST_UMASK", "022")
        .env(
            "WRIX_TEST_IMAGE_DERIVATION",
            binding.parent().ok_or("target parent")?.join("source.drv"),
        )
        .env(
            "WRIX_TEST_FAIL_IMAGE_REALISATION",
            fixture.root.path().join("fail-image"),
        )
        .env(
            "WRIX_TEST_FAIL_BACKEND_START",
            fixture.root.path().join("fail-backend"),
        )
        .env_remove("WRIX_NIX_POOL_PACKAGE_BINDING")
        .env_remove("WRIX_NIX_POOL_HOST_TOOLS");
    fixture
        .controller
        .borrow()
        .as_ref()
        .ok_or("controller")?
        .configure(&mut command)?;
    Ok(command)
}

fn resume_command(fixture: &Fixture) -> TestResult<Command> {
    let mut command = fixture.command()?;
    command
        .args(["service", "update", "--resume"])
        .env("WRIX_TEST_DIGEST", "b".repeat(64))
        .env(
            "WRIX_TEST_FAIL_BACKEND_START",
            fixture.root.path().join("fail-backend"),
        );
    fixture
        .controller
        .borrow()
        .as_ref()
        .ok_or("controller")?
        .configure(&mut command)?;
    Ok(command)
}

#[test]
fn cancellation_archives_the_real_intent_from_failed_image_realisation() -> TestResult {
    let fixture = Fixture::shared_cache()?;
    let binding = bind_target(&fixture)?;
    let state = fixture.root.path().join("state/wrix");
    let lifecycle = state.join("host-lifecycle");
    let runtime_path = fixture.root.path().join("runtime.json");
    let mut peers: Value = serde_json::from_slice(&fs::read(&runtime_path)?)?;
    peers.as_array_mut().ok_or("inventory")?.push(json!({
        "configuration":{"id":"peer-worker","labels":{"wrix.kind":"sandbox"}},
        "status":{"state":"running"}
    }));
    fs::write(&runtime_path, serde_json::to_vec(&peers)?)?;
    let inventory = fs::read(&runtime_path)?;
    let payload = state.join("service-data/celler/fixture-signing-state");
    fs::write(&payload, b"retained signing state")?;
    let owner = fs::read(lifecycle.join("celler-owner.json"))?;
    fs::write(fixture.root.path().join("fail-image"), b"")?;
    let failed = run_command(target_command(&fixture, &binding)?.args(["service", "update"]))?;
    assert!(!failed.status.success());
    assert_eq!(
        fs::read(fixture.root.path().join("fail-image"))?,
        b"realise\n"
    );
    let bytes = fs::read(lifecycle.join("update.json"))?;
    let intent: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(intent["phase"], "prepared");
    let blocked = fixture.run("start")?;
    assert!(!blocked.status.success());
    assert!(
        blocked.stderr.contains("pending service update"),
        "{}",
        blocked.stderr
    );
    let refuse = || -> TestResult {
        let result = run_command(fixture.command()?.args(["service", "update", "--cancel"]))?;
        assert!(!result.status.success());
        assert_eq!(fs::read(lifecycle.join("update.json"))?, bytes);
        assert_eq!(fs::read(&runtime_path)?, inventory);
        Ok(())
    };
    let checkpoints = lifecycle.join("checkpoints");
    fs::DirBuilder::new().mode(0o700).create(&checkpoints)?;
    for marker in [
        state.join("nix-pool-maintenance.json"),
        lifecycle.join("shutdown.json"),
        checkpoints.join(intent["attempt"].as_str().ok_or("attempt")?),
    ] {
        fs::write(&marker, b"uncertain maintenance")?;
        set_mode(&marker, 0o600)?;
        refuse()?;
        fs::remove_file(marker)?;
    }
    let unit = state.join("control/wrix-host-controller.service");
    let original_unit = fs::read(&unit)?;
    fs::write(&unit, b"foreign controller unit")?;
    refuse()?;
    fs::write(unit, original_unit)?;
    for entry in peers.as_array_mut().ok_or("inventory")? {
        if entry["configuration"]["labels"]["wrix.kind"] == "service" {
            entry["status"]["state"] = json!("stopped");
        }
    }
    fs::write(&runtime_path, serde_json::to_vec(&peers)?)?;
    let refused = run_command(fixture.command()?.args(["service", "update", "--cancel"]))?;
    assert!(!refused.status.success());
    assert_eq!(fs::read(lifecycle.join("update.json"))?, bytes);
    fs::write(&runtime_path, &inventory)?;
    let cancelled = run_command(fixture.command()?.args(["service", "update", "--cancel"]))?;
    assert!(cancelled.status.success(), "{}", cancelled.stderr);
    assert_eq!(
        fs::read(lifecycle.join(format!(
            "cancelled-update-{}.json",
            intent["attempt"].as_str().ok_or("attempt")?
        )))?,
        bytes
    );
    assert!(!lifecycle.join("update.json").exists());
    assert_eq!(fs::read(&payload)?, b"retained signing state");
    assert_eq!(fs::read(lifecycle.join("celler-owner.json"))?, owner);
    let retry = run_command(fixture.command()?.args(["service", "update", "--cancel"]))?;
    assert!(retry.status.success(), "{}", retry.stderr);
    let started = fixture.run("start")?;
    assert!(started.status.success(), "{}", started.stderr);
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        inventory
    );
    Ok(())
}

#[test]
fn interrupted_update_resumes_the_checkpoint_produced_by_the_cli() -> TestResult {
    // Only the old-version input is seeded. Wrix produces every shutdown,
    // update, ownership, checkpoint, lease archive and activation artifact.
    let mut fixture = Fixture::legacy_shared_cache()?;
    fs::write(
        &fixture.runtime,
        format!(
            "{}\n{}",
            include_str!("../fixtures/update-resume-runtime.sh"),
            APPLE_RUNTIME
        ),
    )?;
    fixture
        .controller
        .borrow()
        .as_ref()
        .ok_or("controller")?
        .enable_restart()?;
    let binding = bind_target(&fixture)?;
    let state = fixture.root.path().join("state/wrix");
    let lifecycle = state.join("host-lifecycle");
    let databases = state.join("service-data/dolt/databases");
    fs::create_dir_all(&databases)?;
    set_mode(&state.join("service-data/dolt"), 0o700)?;
    fs::write(databases.join("fixture-data"), b"retained database")?;
    let signing = state.join("service-data/celler/fixture-signing-state");
    fs::write(&signing, b"retained signing state")?;
    let locks = [
        state.join("service-data/dolt/owner.lock"),
        state.join("service-data/celler/backend.lock"),
    ];
    for lock in &locks {
        fs::write(lock, b"original owner inode")?;
        set_mode(lock, 0o600)?;
    }
    let inodes = locks
        .iter()
        .map(fs::metadata)
        .collect::<Result<Vec<_>, _>>()?;
    fixture
        .snapshot
        .as_array_mut()
        .ok_or("inventory")?
        .push(json!({
            "configuration":{"id":"peer-worker","labels":{"wrix.kind":"sandbox"}},
            "status":{"state":"running"}
        }));
    fixture.write_snapshot()?;
    let leases_path = state.join("control/authority/role-endpoints.json");
    let prior_leases = fs::read(&leases_path)?;
    let origin: Value = serde_json::from_slice(&fs::read(lifecycle.join("celler-owner.json"))?)?;
    let stopped = run_command(fixture.command()?.args(["service", "stop", "--host"]))?;
    assert!(stopped.status.success(), "{}", stopped.stderr);
    fixture.server.borrow_mut().take();
    fixture.api()?;
    let fault = fixture.root.path().join("fail-backend");
    fs::write(&fault, b"")?;
    let refuse_preparation = || -> TestResult {
        let result = run_command(target_command(&fixture, &binding)?.args(["service", "update"]))?;
        assert!(!result.status.success());
        let pending: Value = serde_json::from_slice(&fs::read(lifecycle.join("update.json"))?)?;
        assert_eq!(pending["phase"], "preparing");
        assert_eq!(fs::read(&leases_path)?, prior_leases);
        assert!(!state.join("control/restart.request").exists());
        assert_eq!(fs::read(&fault)?, b"");
        Ok(())
    };
    let busy = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&locks[0])?;
    fs2::FileExt::lock_exclusive(&busy)?;
    refuse_preparation()?;
    drop(busy);
    let saved_lock = locks[0].with_file_name("saved-owner.lock");
    fs::rename(&locks[0], &saved_lock)?;
    fs::write(&locks[0], b"different owner inode")?;
    set_mode(&locks[0], 0o600)?;
    refuse_preparation()?;
    fs::remove_file(&locks[0])?;
    fs::rename(saved_lock, &locks[0])?;
    fs::rename(
        lifecycle.join("shutdown.json"),
        lifecycle.join("saved-shutdown"),
    )?;
    refuse_preparation()?;
    fs::rename(
        lifecycle.join("saved-shutdown"),
        lifecycle.join("shutdown.json"),
    )?;
    let mut command = target_command(&fixture, &binding)?;
    command.args(["service", "update"]);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let restarted = fixture
        .controller
        .borrow_mut()
        .as_mut()
        .ok_or("controller")?
        .restart_when_requested(&mut child);
    if restarted.is_err() && child.try_wait()?.is_none() {
        child.kill()?;
    }
    let output = child.wait_with_output()?;
    assert!(
        restarted.is_ok(),
        "{restarted:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success());
    assert_eq!(
        fs::read(&fault)?,
        b"run\n",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(lifecycle.join("update.json"))?;
    let intent: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(intent["phase"], "starting");
    let checkpoints = lifecycle.join("checkpoints");
    let checkpoint = checkpoints.join(intent["attempt"].as_str().ok_or("attempt")?);
    let produced_mode = fs::metadata(&checkpoint)?.mode() & 0o777;
    let receipt_path = checkpoint.join("checkpoint.json");
    let receipt = fs::read(&receipt_path)?;
    assert_eq!(
        fs::read(checkpoint.join("dolt/fixture-data"))?,
        b"retained database"
    );
    assert_eq!(
        fs::read(checkpoint.join("celler/fixture-signing-state"))?,
        b"retained signing state"
    );
    assert!(!checkpoint.join("celler/backend.lock").exists());
    // Historical tempfile producers emitted 0755 under umask 022. Retain the
    // actual receipt/data; change only the compatibility input's directory mode.
    set_mode(&checkpoint, 0o755)?;
    let retried = run_command(&mut resume_command(&fixture)?)?;
    assert!(!retried.status.success());
    assert_eq!(
        fs::read(&fault)?,
        b"run\nrun\n",
        "resume did not consume the producer's checkpoint (produced mode {produced_mode:o}): {}",
        retried.stderr
    );
    assert_eq!(
        produced_mode, 0o700,
        "new checkpoints must be private even with umask 022"
    );
    let refuse = || -> TestResult {
        let result = run_command(&mut resume_command(&fixture)?)?;
        assert!(!result.status.success());
        assert!(result.stderr.contains("checkpoint"), "{}", result.stderr);
        assert_eq!(fs::read(&fault)?, b"run\nrun\n");
        assert_eq!(fs::read(lifecycle.join("update.json"))?, bytes);
        Ok(())
    };
    set_mode(&checkpoint, 0o777)?;
    refuse()?;
    set_mode(&checkpoint, 0o755)?;
    set_mode(&checkpoints, 0o755)?;
    refuse()?;
    set_mode(&checkpoints, 0o700)?;
    let alias = checkpoint.join("receipt-alias");
    fs::hard_link(&receipt_path, &alias)?;
    refuse()?;
    fs::remove_file(alias)?;
    fs::rename(&receipt_path, checkpoint.join("saved-receipt"))?;
    refuse()?;
    fs::rename(checkpoint.join("saved-receipt"), &receipt_path)?;
    for field in ["version", "target", "digest", "source"] {
        let mut changed: Value = serde_json::from_slice(&receipt)?;
        changed[field] = match field {
            "version" => json!(99),
            "target" => {
                serde_json::from_slice(&fs::read(fixture.root.path().join("binding.json"))?)?
            }
            _ => json!(format!("sha256:{}", "c".repeat(64))),
        };
        fs::write(&receipt_path, serde_json::to_vec(&changed)?)?;
        refuse()?;
    }
    fs::write(&receipt_path, &receipt)?;
    let saved = checkpoints.join("saved-checkpoint");
    fs::rename(&checkpoint, &saved)?;
    std::os::unix::fs::symlink(&saved, &checkpoint)?;
    refuse()?;
    fs::remove_file(&checkpoint)?;
    fs::rename(saved, &checkpoint)?;
    fs::remove_file(&fault)?;
    // Deliberately use the caller's original, different binding. --resume must
    // use the recorded target, not quietly supersede it with this package.
    let resumed = run_command(&mut resume_command(&fixture)?)?;
    assert!(resumed.status.success(), "{}", resumed.stderr);
    assert!(!lifecycle.join("update.json").exists());
    assert!(!lifecycle.join("shutdown.json").exists());
    assert_eq!(fs::read(&receipt_path)?, receipt);
    assert_eq!(fs::metadata(&checkpoint)?.mode() & 0o777, 0o755);
    assert_eq!(
        fs::read(databases.join("fixture-data"))?,
        b"retained database"
    );
    assert_eq!(fs::read(signing)?, b"retained signing state");
    for (lock, original) in locks.iter().zip(inodes) {
        let current = fs::metadata(lock)?;
        assert_eq!(
            (current.dev(), current.ino()),
            (original.dev(), original.ino())
        );
    }
    let selected: Value = serde_json::from_slice(&fs::read(lifecycle.join("celler-owner.json"))?)?;
    assert_eq!(selected["recipe"]["binding"], intent["candidate"]);
    assert_eq!(selected["cache_http_port"], origin["cache_http_port"]);
    let leases: Value = serde_json::from_slice(&fs::read(leases_path)?)?;
    assert_eq!(leases["ports"][1], origin["cache_http_port"]);
    let archives = fs::read_dir(state.join("control/authority"))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("role-endpoints-before-")
        })
        .collect::<Vec<_>>();
    assert_eq!(archives.len(), 1);
    assert_eq!(fs::read(archives[0].path())?, prior_leases);
    let inventory = fs::read(fixture.root.path().join("runtime.json"))?;
    let peers: Value = serde_json::from_slice(&inventory)?;
    assert_eq!(
        peers
            .as_array()
            .ok_or("inventory")?
            .iter()
            .find(|entry| entry["configuration"]["id"] == "peer-worker")
            .ok_or("peer disappeared")?["status"]["state"],
        "running"
    );
    let no_pending = run_command(fixture.command()?.args(["service", "update", "--resume"]))?;
    assert!(!no_pending.status.success());
    assert!(
        no_pending.stderr.contains("no interrupted service update"),
        "{}",
        no_pending.stderr
    );
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        inventory
    );
    Ok(())
}
