//! Production [`AgentLoopController`] used by the `loom run` binary.
//!
//! Wires `BdClient` for bead lookup/close/clarify, a `tokio::process::Command`
//! shell-out for `exec_check`, and a caller-provided dispatch closure for the
//! actual agent invocation. The closure pattern keeps backend selection
//! (`PiBackend` vs `ClaudeBackend`) inside the binary's `dispatch` match —
//! `loom-workflow` never sees the concrete backend types, mirroring the shape
//! used by `ProductionTodoController` and `run_parallel_batch`.
//!
//! Per-bead profile dispatch is wired through [`build_spawn_config_from_manifest`]:
//! the manifest, CLI `--profile` override, and per-phase fallback all flow
//! into the controller at construction time so `run_bead` resolves the
//! per-bead `image_ref` + `image_source` against the parsed manifest before
//! the agent invocation. A missing manifest entry surfaces as
//! [`RunError::Profile`] — no silent fallback.

use std::path::PathBuf;
use std::sync::Arc;

use loom_core::agent::{ProtocolError, RePinContent, SessionOutcome, SpawnConfig};
use loom_core::bd::{BdClient, Bead, ListOpts, ReadyOpts, UpdateOpts};
use loom_core::identifier::{BeadId, ProfileName, SpecLabel};
use loom_core::lock::LockGuard;
use loom_core::profile_manifest::ProfileImageManifest;
use tokio::process::Command;
use tracing::info;

use super::error::RunError;
use super::outcome::AgentOutcome;
use super::runner::AgentLoopController;
use super::spawn::build_spawn_config_from_manifest;

/// Wires the [`AgentLoopController`] trait against the real `BdClient`, a
/// caller-provided agent dispatch closure, and a child `loom check` exec for
/// handoff.
///
/// `manifest` / `cli_profile` / `phase_default` are the inputs the per-bead
/// profile resolver chain needs (see
/// [`super::resolve_profile_image`]). They are stored on the controller so
/// every `run_bead` call resolves the bead's `image_ref` + `image_source`
/// from the same parsed manifest, never re-reading it from disk.
///
/// `spawn` is the per-phase dispatch closure: the binary builds it from
/// `dispatch(kind, &spawn_config)` so the workflow stays backend-agnostic.
/// `run_bead` calls it on every retry attempt, so the closure must be `Fn`
/// (callable repeatedly). It receives `(SpawnConfig, BeadId)` — the bead id
/// is passed alongside the spawn config so the closure can open the per-bead
/// JSONL [`LogSink`](loom_core::logging::LogSink) before dispatch.
pub struct ProductionAgentLoopController<S, F>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = Result<SessionOutcome, ProtocolError>> + Send,
{
    bd: BdClient,
    label: SpecLabel,
    loom_bin: PathBuf,
    workspace: PathBuf,
    manifest: Arc<ProfileImageManifest>,
    cli_profile: Option<ProfileName>,
    phase_default: ProfileName,
    spawn: S,
    /// Spec lock dropped before exec'ing `loom check` so the child can take it.
    lock: Option<LockGuard>,
}

impl<S, F> ProductionAgentLoopController<S, F>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = Result<SessionOutcome, ProtocolError>> + Send,
{
    #[expect(clippy::too_many_arguments, reason = "controller construction surface")]
    pub fn new(
        bd: BdClient,
        label: SpecLabel,
        loom_bin: PathBuf,
        workspace: PathBuf,
        manifest: Arc<ProfileImageManifest>,
        cli_profile: Option<ProfileName>,
        phase_default: ProfileName,
        spawn: S,
    ) -> Self {
        Self {
            bd,
            label,
            loom_bin,
            workspace,
            manifest,
            cli_profile,
            phase_default,
            spawn,
            lock: None,
        }
    }

    /// Hand the spec lock to the controller so `exec_check` can drop it
    /// before spawning the `loom check` child (which acquires the same lock).
    pub fn with_handoff_lock(mut self, guard: LockGuard) -> Self {
        self.lock = Some(guard);
        self
    }

    fn spec_label_filter(&self) -> String {
        format!("spec:{}", self.label.as_str())
    }
}

impl<S, F> AgentLoopController for ProductionAgentLoopController<S, F>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = Result<SessionOutcome, ProtocolError>> + Send,
{
    async fn next_ready_bead(&mut self) -> Result<Option<Bead>, RunError> {
        let beads = self
            .bd
            .ready(ReadyOpts {
                limit: Some(1),
                label: Some(self.spec_label_filter()),
            })
            .await?;
        Ok(beads.into_iter().next())
    }

    async fn run_bead(
        &mut self,
        bead: &Bead,
        previous_failure: Option<String>,
    ) -> Result<AgentOutcome, RunError> {
        let initial_prompt = format!("loom run: bead {}", bead.id);
        let banner = format!("loom run @ {}", bead.id);
        let scratch = loom_core::scratch::ScratchSession::open(
            &self.workspace,
            bead.id.as_str(),
            &initial_prompt,
            &banner,
        )
        .map_err(|source| RunError::Protocol(ProtocolError::Io(source)))?;
        let spawn_config = build_spawn_config_from_manifest(
            &self.manifest,
            bead,
            self.cli_profile.as_ref(),
            &self.phase_default,
            self.workspace.clone(),
            initial_prompt,
            RePinContent {
                orientation: banner,
                pinned_context: String::new(),
                partial_bodies: vec![],
            },
            scratch.path().to_path_buf(),
            vec![],
            vec![],
        )?;
        info!(
            bead = %bead.id,
            image_ref = %spawn_config.image_ref,
            retry = previous_failure.is_some(),
            "loom run: dispatching agent",
        );
        let outcome = (self.spawn)(spawn_config, bead.id.clone()).await;
        // Drop happens here at end of scope — scratch dir cleaned up on
        // every exit path (success, failure, panic).
        drop(scratch);
        let outcome = outcome?;
        if outcome.exit_code == 0 {
            Ok(AgentOutcome::Success)
        } else {
            Ok(AgentOutcome::Failure {
                error: format!("agent exited with code {}", outcome.exit_code),
            })
        }
    }

    async fn close_bead(&mut self, bead: &BeadId) -> Result<(), RunError> {
        self.bd.close(bead, None).await?;
        Ok(())
    }

    async fn apply_clarify(&mut self, bead: &BeadId) -> Result<(), RunError> {
        self.bd
            .update(
                bead,
                UpdateOpts {
                    add_labels: vec!["loom:clarify".to_string()],
                    ..UpdateOpts::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn exec_check(&mut self) -> Result<(), RunError> {
        // Release the spec lock before spawning the child — `loom check`
        // acquires the same lock and would otherwise time out behind us.
        self.lock.take();
        let status = Command::new(&self.loom_bin)
            .current_dir(&self.workspace)
            .arg("check")
            .arg("-s")
            .arg(self.label.as_str())
            .status()
            .await?;
        if !status.success() {
            return Err(RunError::CheckHandoff(status.to_string()));
        }
        Ok(())
    }
}

/// Helper used by `main.rs` to fetch the spec-filtered open list when the
/// caller needs the typed [`Bead`] slice (e.g. to print a status line).
/// Surfacing this here keeps the BdClient list-shape next to the controller.
pub async fn list_open_for_spec(bd: &BdClient, label: &SpecLabel) -> Result<Vec<Bead>, RunError> {
    let beads = bd
        .list(ListOpts {
            status: Some("open".to_string()),
            label: Some(format!("spec:{}", label.as_str())),
        })
        .await?;
    Ok(beads)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;
    use loom_core::bd::Label;
    use std::sync::Mutex;

    fn write_manifest(dir: &std::path::Path) -> Arc<ProfileImageManifest> {
        let body = r#"{
          "base": { "ref": "localhost/wrapix-base:abc", "source": "/nix/store/aaa-image-base" }
        }"#;
        let path = dir.join("profile-images.json");
        std::fs::write(&path, body).expect("write manifest");
        Arc::new(ProfileImageManifest::from_path(&path).expect("parse manifest"))
    }

    fn bead(id: &str) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: format!("title-{id}"),
            description: "desc".into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![Label::new("profile:base")],
        }
    }

    #[tokio::test]
    async fn run_bead_invokes_dispatch_closure_with_resolved_spawn_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let captured: Arc<Mutex<Option<SpawnConfig>>> = Arc::new(Mutex::new(None));
        let captured_for_closure = Arc::clone(&captured);
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            PathBuf::from("/workspace"),
            manifest,
            None,
            ProfileName::new("base"),
            move |cfg: SpawnConfig, _bead_id: BeadId| {
                let captured = Arc::clone(&captured_for_closure);
                async move {
                    *captured.lock().unwrap() = Some(cfg);
                    Ok(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    })
                }
            },
        );
        let outcome = controller
            .run_bead(&bead("wx-1"), None)
            .await
            .expect("run_bead ok");
        assert_eq!(outcome, AgentOutcome::Success);
        let cfg = captured.lock().unwrap().take().expect("closure called");
        assert_eq!(cfg.image_ref, "localhost/wrapix-base:abc");
        assert!(cfg.initial_prompt.contains("wx-1"));
    }

    #[tokio::test]
    async fn run_bead_translates_nonzero_exit_code_into_failure_with_error_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            PathBuf::from("/workspace"),
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                Ok(SessionOutcome {
                    exit_code: 42,
                    cost_usd: None,
                })
            },
        );
        let outcome = controller
            .run_bead(&bead("wx-2"), None)
            .await
            .expect("run_bead ok");
        match outcome {
            AgentOutcome::Failure { error } => {
                assert!(
                    error.contains("42"),
                    "error body should mention exit code 42: {error}"
                );
            }
            AgentOutcome::Success => panic!("non-zero exit must produce Failure"),
        }
    }

    #[tokio::test]
    async fn run_bead_surfaces_protocol_error_through_run_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            PathBuf::from("/workspace"),
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move { Err(ProtocolError::Unsupported) },
        );
        let result = controller.run_bead(&bead("wx-3"), None).await;
        match result {
            Err(RunError::Protocol(ProtocolError::Unsupported)) => {}
            other => panic!("expected Protocol(Unsupported), got {other:?}"),
        }
    }

    /// Regression: `loom run` used to hold the spec lock for its whole
    /// lifetime, so the `loom check` child it spawned at the molecule-complete
    /// handoff timed out trying to acquire the same lock. `exec_check` must
    /// drop the held [`LockGuard`] before spawning, leaving the kernel-level
    /// `flock(2)` available to the child. Verified end-to-end: after a stub
    /// child exits, the lock is reacquirable on a fresh attempt.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_check_releases_lock_before_spawning_child() {
        use loom_core::clock::SystemClock;
        use loom_core::lock::LockManager;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let mgr = LockManager::new(dir.path()).expect("lock manager");
        let label = SpecLabel::new("alpha");
        let clock = SystemClock::new();
        let guard = mgr
            .acquire_spec_async(&label, &clock)
            .await
            .expect("first acquire");

        // Stand-in for the `loom` binary: ignores all args and exits 0.
        // /bin/true does not exist on NixOS, so we ship a script.
        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            label.clone(),
            stub,
            dir.path().to_path_buf(),
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                Ok(SessionOutcome {
                    exit_code: 0,
                    cost_usd: None,
                })
            },
        )
        .with_handoff_lock(guard);

        controller.exec_check().await.expect("exec_check ok");

        // The child has exited and the controller's guard was dropped before
        // the spawn — the lock must be free. A short timeout keeps the test
        // fast on the regression (held-lock) path: it would error in <100ms
        // rather than wait the default 5s.
        let _reacquired = mgr
            .acquire_spec_with_timeout_async(&label, &clock, Duration::from_millis(100))
            .await
            .expect("lock must be reacquirable after exec_check");
    }
}
