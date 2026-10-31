use super::*;
#[path = "controller_restart.rs"]
mod controller_restart;
use std::process::Stdio;

#[test]
fn current_cli_resumes_checkpointed_legacy_target_and_repairs_retained_endpoints() -> TestResult {
    resume_case(true)
}

#[test]
fn current_cli_preserves_legacy_endpoints_before_first_release() -> TestResult {
    resume_case(false)
}

fn resume_case(released: bool) -> TestResult {
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
        .borrow_mut()
        .as_mut()
        .ok_or("controller")?
        .enable_restart()?;
    let state = fixture.root.path().join("state/wrix");
    set_mode(&state.join("service-data/dolt"), 0o700)?;
    for path in [
        state.join("service-data/dolt/owner.lock"),
        state.join("service-data/celler/backend.lock"),
    ] {
        fs::write(&path, b"original owner inode")?;
        set_mode(&path, 0o600)?;
    }
    let data = state.join("service-data/dolt/native-payload");
    fs::write(&data, b"retained database")?;
    let signing = fixture.state_root()?.join("keys/cache.secret");
    let secret = fs::read(&signing)?;
    fixture
        .snapshot
        .as_array_mut()
        .ok_or("inventory")?
        .push(json!({
            "configuration":{"id":"peer-worker","labels":{"wrix.kind":"sandbox"}},
            "status":{"state":"running"}
        }));
    fixture.write_snapshot()?;
    let running_remove = fixture
        .runtime_command()?
        .args([
            "rm",
            fixture.metadata["container_name"]
                .as_str()
                .ok_or("container")?,
        ])
        .output()?;
    assert!(!running_remove.status.success());
    let stopped = run_command(fixture.command()?.args(["service", "stop", "--host"]))?;
    assert!(stopped.status.success(), "{}", stopped.stderr);
    fixture.server.borrow_mut().take();
    let lifecycle = state.join("host-lifecycle");
    let shutdown: Value = serde_json::from_slice(&fs::read(lifecycle.join("shutdown.json"))?)?;
    assert_eq!(shutdown["phase"], "stopped");
    let origin: Value = serde_json::from_slice(&fs::read(lifecycle.join("celler-owner.json"))?)?;
    if released {
        let removed = fixture
            .runtime_command()?
            .args([
                "rm",
                fixture.metadata["container_name"]
                    .as_str()
                    .ok_or("container")?,
            ])
            .output()?;
        assert!(
            removed.status.success(),
            "{}",
            String::from_utf8_lossy(&removed.stderr)
        );
    }
    fixture.snapshot =
        serde_json::from_slice(&fs::read(fixture.root.path().join("runtime.json"))?)?;
    fixture.api()?;
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
    let attempt = "f".repeat(32);
    let pending = json!({
        "version":2,"deployment":shutdown["deployment"],"attempt":attempt,
        "candidate":candidate,
        "controller":serde_json::from_slice::<Value>(&fs::read(state.join("control/package.json"))?)?,
        "pools":{"state":"unprovisioned"}, "phase":if released { "starting" } else { "preparing" },
        "ownership":if released { json!({"locks":shutdown["locks"],"origin":origin["recipe"]["image_id"]}) } else { Value::Null }
    });
    let bytes = serde_json::to_vec(&pending)?;
    fs::write(lifecycle.join("update.json"), &bytes)?;
    set_mode(&lifecycle.join("update.json"), 0o600)?;
    let checkpoint = lifecycle.join("checkpoints").join(&attempt);
    let receipt = if released {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&checkpoint)?;
        fs::write(
            checkpoint.join("checkpoint.json"),
            serde_json::to_vec(&json!({
                "version":1,"target":candidate,"digest":format!("sha256:{}", "b".repeat(64)),
                "source":origin["recipe"]["image_id"]
            }))?,
        )?;
        set_mode(&checkpoint.join("checkpoint.json"), 0o600)?;
        Some(fs::read(checkpoint.join("checkpoint.json"))?)
    } else {
        None
    };
    let leases_path = state.join("control/authority/role-endpoints.json");
    let mut wrong: Value = serde_json::from_slice(&fs::read(&leases_path)?)?;
    wrong["ports"][1] = json!(if origin["cache_http_port"] == 21_000 {
        21_001
    } else {
        21_000
    });
    fs::write(&leases_path, serde_json::to_vec(&wrong)?)?;
    let prior = fs::read(&leases_path)?;
    let mismatch = run_command(fixture.command()?.args(["service", "update"]))?;
    assert!(!mismatch.status.success());
    assert!(
        mismatch
            .stderr
            .contains("different exact package/image intent"),
        "{}",
        mismatch.stderr
    );
    assert_eq!(fs::read(lifecycle.join("update.json"))?, bytes);
    let refuse = |fixture: &Fixture| -> TestResult {
        let result = run_command(
            fixture
                .command()?
                .args(["service", "update", "--resume"])
                .env("WRIX_TEST_DIGEST", "b".repeat(64)),
        )?;
        assert!(!result.status.success(), "unsafe recovery was accepted");
        assert_eq!(fs::read(lifecycle.join("update.json"))?, bytes);
        assert_eq!(fs::read(&leases_path)?, prior);
        assert!(!state.join("control/restart.request").exists());
        assert_eq!(fs::read(&data)?, b"retained database");
        Ok(())
    };
    if let Some(receipt) = &receipt {
        fs::rename(
            checkpoint.join("checkpoint.json"),
            checkpoint.join("saved-receipt"),
        )?;
        refuse(&fixture)?;
        fs::rename(
            checkpoint.join("saved-receipt"),
            checkpoint.join("checkpoint.json"),
        )?;
        for field in ["target", "digest", "source"] {
            let mut changed: Value = serde_json::from_slice(receipt)?;
            changed[field] = if field == "target" {
                serde_json::from_slice(&fs::read(fixture.root.path().join("binding.json"))?)?
            } else {
                json!(format!("sha256:{}", "c".repeat(64)))
            };
            fs::write(
                checkpoint.join("checkpoint.json"),
                serde_json::to_vec(&changed)?,
            )?;
            refuse(&fixture)?;
        }
        fs::write(checkpoint.join("checkpoint.json"), receipt)?;
    }
    fs::rename(
        lifecycle.join("shutdown.json"),
        lifecycle.join("saved-shutdown"),
    )?;
    refuse(&fixture)?;
    fs::rename(
        lifecycle.join("saved-shutdown"),
        lifecycle.join("shutdown.json"),
    )?;
    let lock = state.join("service-data/dolt/owner.lock");
    let saved_lock = state.join("service-data/dolt/saved-owner.lock");
    fs::rename(&lock, &saved_lock)?;
    fs::write(&lock, b"different owner")?;
    set_mode(&lock, 0o600)?;
    refuse(&fixture)?;
    fs::remove_file(&lock)?;
    fs::rename(saved_lock, &lock)?;
    let busy = fs::OpenOptions::new().read(true).write(true).open(&lock)?;
    fs2::FileExt::lock_exclusive(&busy)?;
    refuse(&fixture)?;
    drop(busy);
    let plan = state.join("control/authority/native-plan.json");
    fs::write(&plan, b"uncertain native generation")?;
    set_mode(&plan, 0o600)?;
    refuse(&fixture)?;
    fs::remove_file(plan)?;
    let mut command = fixture.command()?;
    command
        .args(["service", "update", "--resume"])
        .env("WRIX_TEST_DIGEST", "b".repeat(64));
    fixture
        .controller
        .borrow()
        .as_ref()
        .ok_or("controller")?
        .configure(&mut command)?;
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
    let result = child.wait_with_output()?;
    assert!(
        restarted.is_ok(),
        "{restarted:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!lifecycle.join("update.json").exists());
    assert!(!lifecycle.join("shutdown.json").exists());
    let resumed_inventory = fs::read(fixture.root.path().join("runtime.json"))?;
    let no_pending = run_command(fixture.command()?.args(["service", "update", "--resume"]))?;
    assert!(!no_pending.status.success());
    assert!(
        no_pending.stderr.contains("no interrupted service update"),
        "{}",
        no_pending.stderr
    );
    assert!(!lifecycle.join("update.json").exists());
    assert_eq!(
        fs::read(fixture.root.path().join("runtime.json"))?,
        resumed_inventory
    );
    let selected: Value = serde_json::from_slice(&fs::read(lifecycle.join("celler-owner.json"))?)?;
    assert_eq!(selected["recipe"]["binding"], candidate);
    assert_eq!(selected["cache_http_port"], origin["cache_http_port"]);
    let leases: Value = serde_json::from_slice(&fs::read(
        state.join("control/authority/role-endpoints.json"),
    )?)?;
    assert_eq!(leases["ports"][1], origin["cache_http_port"]);
    assert_eq!(fs::read(data)?, b"retained database");
    assert_eq!(fs::read(signing)?, secret);
    let completed = fs::read(checkpoint.join("checkpoint.json"))?;
    if let Some(receipt) = receipt {
        assert_eq!(completed, receipt);
    } else {
        let created: Value = serde_json::from_slice(&completed)?;
        assert_eq!(created["target"], candidate);
        assert_eq!(created["source"], origin["recipe"]["image_id"]);
    }
    let archives = fs::read_dir(state.join("control/authority"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, std::io::Error>>()?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("role-endpoints-before-"))
        })
        .collect::<Vec<_>>();
    assert_eq!(archives.len(), 1);
    assert_eq!(fs::read(&archives[0])?, prior);
    let inventory: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("runtime.json"))?)?;
    assert_eq!(
        inventory
            .as_array()
            .ok_or("inventory must be an array")?
            .iter()
            .find(|entry| entry["configuration"]["id"] == "peer-worker")
            .ok_or("peer worker disappeared")?["status"]["state"],
        "running"
    );
    Ok(())
}
