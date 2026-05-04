//! Integration tests for `loom_core::lock::LockManager`.
//!
//! Each test name maps onto a shell-level acceptance test in
//! `tests/loom-test.sh::test_*`. The shell harness invokes these via
//! `cargo test -p loom-core --test lock_manager <name>`, so the verify path
//! exercises the same code as `cargo test`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use loom_core::identifier::SpecLabel;
use loom_core::lock::{LockError, LockManager};

/// Serializes tests that fork while another test is mid drop+reacquire of a
/// spec lock — the child inherits the fd and `flock(2)` is per-OFD, so the
/// "released" lock still appears held until execve closes CLOEXEC fds.
static FORK_SERIALIZE: Mutex<()> = Mutex::new(());

#[test]
fn acquire_spec_creates_lock_file() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mgr = LockManager::new(dir.path())?;
    let label = SpecLabel::new("alpha");

    let _guard = mgr.acquire_spec(&label)?;

    let lock_path = mgr.locks_dir().join("alpha.lock");
    if !lock_path.is_file() {
        return Err(anyhow!("expected lock file at {}", lock_path.display()));
    }
    Ok(())
}

#[test]
fn second_acquire_times_out_with_spec_busy() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mgr = LockManager::new(dir.path())?;
    let label = SpecLabel::new("contended");

    // First acquisition holds the lock for the whole test.
    let _holder = mgr.acquire_spec(&label)?;

    // Use a sub-second timeout for the test itself; the *default* 5-second
    // wait is exercised by `times_out_with_default_timeout` so this test
    // doesn't add 5s to every cargo run.
    let timeout = Duration::from_millis(250);
    let started = Instant::now();
    let result = mgr.acquire_spec_with_timeout(&label, timeout);
    let waited = started.elapsed();

    match result {
        Err(LockError::SpecBusy { label: ref l }) if l == "contended" => {}
        other => return Err(anyhow!("expected SpecBusy(contended), got {other:?}")),
    }
    if waited < timeout {
        return Err(anyhow!(
            "second acquire returned early ({waited:?}) — should wait the full timeout"
        ));
    }
    Ok(())
}

#[test]
fn times_out_with_default_timeout() -> Result<()> {
    // Smoke-check the default 5s wait advertised in the spec without
    // dragging every test run by 5s. We use the explicit default by relying
    // on `acquire_spec` (no override) but only assert directionally.
    let dir = tempfile::tempdir()?;
    let mgr = LockManager::new(dir.path())?;
    let label = SpecLabel::new("default-timeout");
    let _holder = mgr.acquire_spec(&label)?;

    let started = Instant::now();
    let result = mgr.acquire_spec(&label);
    let waited = started.elapsed();

    match result {
        Err(LockError::SpecBusy { .. }) => {}
        other => return Err(anyhow!("expected SpecBusy, got {other:?}")),
    }
    // The default is 5s; allow some scheduler slop on either side.
    if waited < Duration::from_millis(4_500) {
        return Err(anyhow!("default timeout wait too short: {waited:?}"));
    }
    if waited > Duration::from_millis(7_000) {
        return Err(anyhow!("default timeout wait too long: {waited:?}"));
    }
    Ok(())
}

#[test]
fn cross_spec_locks_do_not_block() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mgr = LockManager::new(dir.path())?;
    let alpha = SpecLabel::new("alpha");
    let beta = SpecLabel::new("beta");

    let _alpha_guard = mgr.acquire_spec(&alpha)?;

    let started = Instant::now();
    let _beta_guard = mgr.acquire_spec(&beta)?;
    let waited = started.elapsed();

    if waited > Duration::from_millis(250) {
        return Err(anyhow!(
            "cross-spec acquire blocked unexpectedly: {waited:?}"
        ));
    }
    Ok(())
}

#[test]
fn readonly_paths_unaffected_by_spec_lock() -> Result<()> {
    // Read-only commands (status, logs, spec) acquire no lock. We cannot
    // test the CLI from the lock module directly, but we verify the property
    // at this layer: holding a spec lock does not impede unrelated workspace
    // file I/O or a separate `LockManager::new` for inspection.
    let dir = tempfile::tempdir()?;
    let mgr = LockManager::new(dir.path())?;
    let label = SpecLabel::new("active-run");
    let _guard = mgr.acquire_spec(&label)?;

    // Re-open the manager (a read-only command would do the same to inspect
    // the locks dir) — must not block or error.
    let started = Instant::now();
    let mgr2 = LockManager::new(dir.path())?;
    let _ignored = mgr2.locks_dir().is_dir();
    let waited = started.elapsed();
    if waited > Duration::from_millis(100) {
        return Err(anyhow!("readonly inspection blocked: {waited:?}"));
    }

    // Reading any file in the workspace must work even while the spec lock
    // is held — flock(2) on the lock file does not propagate to siblings.
    let payload = dir.path().join("README");
    std::fs::write(&payload, "hello")?;
    let body = std::fs::read_to_string(&payload)?;
    if body != "hello" {
        return Err(anyhow!("workspace read returned wrong content: {body:?}"));
    }
    Ok(())
}

#[test]
fn acquire_workspace_errors_when_spec_lock_held() -> Result<()> {
    let _serialize = FORK_SERIALIZE.lock().expect("FORK_SERIALIZE poisoned");
    let dir = tempfile::tempdir()?;
    let mgr = LockManager::new(dir.path())?;

    // With no spec lock held, workspace acquires cleanly.
    {
        let _ws = mgr.acquire_workspace()?;
    }

    let label = SpecLabel::new("busy-spec");
    let _spec = mgr.acquire_spec(&label)?;
    match mgr.acquire_workspace() {
        Err(LockError::WorkspaceBusy { label: ref l }) if l == "busy-spec" => {}
        other => return Err(anyhow!("expected WorkspaceBusy(busy-spec), got {other:?}")),
    }

    drop(_spec);
    let _ws_again = mgr.acquire_workspace()?;
    Ok(())
}

#[test]
fn acquire_workspace_serializes_workspace_holders() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mgr = LockManager::new(dir.path())?;

    let _first = mgr.acquire_workspace()?;
    match mgr.acquire_workspace() {
        Err(LockError::WorkspaceBusy { label: ref l }) if l == "workspace" => Ok(()),
        other => Err(anyhow!("expected WorkspaceBusy(workspace), got {other:?}")),
    }
}

/// Helper test — invoked as a child process by `crash_releases_spec_lock`.
/// `#[ignore]` so plain `cargo test` does not run it as part of the suite.
/// The parent passes `LOOM_LOCK_TEST_DIR` to point the child at a tempdir.
#[test]
#[ignore]
fn crash_helper_take_lock_then_exit() -> Result<()> {
    let workspace = std::env::var("LOOM_LOCK_TEST_DIR")?;
    let label_str = std::env::var("LOOM_LOCK_TEST_LABEL")?;
    let mgr = LockManager::new(PathBuf::from(&workspace))?;
    let _guard = mgr.acquire_spec(&SpecLabel::new(label_str))?;
    // Exit without unwinding: kernel closes the open fd, releasing flock.
    // This is the same effect as a SIGKILL'd process — proves the spec
    // claim that crashed processes leave no stale locks.
    std::process::exit(0);
}

#[test]
fn crash_releases_spec_lock() -> Result<()> {
    let _serialize = FORK_SERIALIZE.lock().expect("FORK_SERIALIZE poisoned");
    let dir = tempfile::tempdir()?;
    let workspace = dir.path().to_path_buf();
    let label = "crash-test";

    let exe = std::env::current_exe()?;
    let status = Command::new(&exe)
        .env("LOOM_LOCK_TEST_DIR", &workspace)
        .env("LOOM_LOCK_TEST_LABEL", label)
        .args([
            "--ignored",
            "--exact",
            "crash_helper_take_lock_then_exit",
            "--nocapture",
        ])
        .status()?;
    if !status.success() {
        return Err(anyhow!("crash helper exited non-zero: {status:?}"));
    }

    // Child process has fully exited; the kernel released its flock when
    // the fd closed at exit. A fresh acquire must succeed immediately.
    let mgr = LockManager::new(&workspace)?;
    let started = Instant::now();
    let _guard = mgr.acquire_spec(&SpecLabel::new(label))?;
    let waited = started.elapsed();
    if waited > Duration::from_millis(250) {
        return Err(anyhow!(
            "post-crash acquire took {waited:?} — expected immediate"
        ));
    }
    Ok(())
}

#[test]
fn second_thread_unblocks_when_holder_drops() -> Result<()> {
    // Sanity: serialization works across threads, and once the first holder
    // releases the lock, the waiter completes.
    let dir = tempfile::tempdir()?;
    let workspace = dir.path().to_path_buf();
    let label = SpecLabel::new("handoff");
    let mgr = LockManager::new(&workspace)?;

    let holder = mgr.acquire_spec(&label)?;

    let (tx, rx) = mpsc::channel::<Result<Duration, String>>();
    let label_clone = label.clone();
    let workspace_clone = workspace.clone();
    let waiter = thread::spawn(move || {
        let mgr2 = match LockManager::new(&workspace_clone) {
            Ok(m) => m,
            Err(e) => {
                let _ = tx.send(Err(format!("manager: {e}")));
                return;
            }
        };
        let started = Instant::now();
        match mgr2.acquire_spec_with_timeout(&label_clone, Duration::from_secs(3)) {
            Ok(_g) => {
                let _ = tx.send(Ok(started.elapsed()));
            }
            Err(e) => {
                let _ = tx.send(Err(format!("acquire: {e}")));
            }
        }
    });

    // Give the waiter time to begin polling.
    thread::sleep(Duration::from_millis(150));
    drop(holder);

    let elapsed = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(d)) => d,
        Ok(Err(msg)) => return Err(anyhow!("waiter failed: {msg}")),
        Err(e) => return Err(anyhow!("waiter timed out: {e}")),
    };
    waiter
        .join()
        .map_err(|_| anyhow!("waiter thread panicked"))?;

    if elapsed > Duration::from_millis(800) {
        return Err(anyhow!("handoff took too long: {elapsed:?}"));
    }
    Ok(())
}
