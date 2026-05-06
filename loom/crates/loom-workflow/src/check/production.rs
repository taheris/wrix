//! Production [`CheckController`] used by the `loom check` binary.
//!
//! Wires `BdClient` for spec-bead snapshots and clarify,
//! `tokio::process::Command` shell-outs for `git push`, `beads-push`, and
//! the auto-iterate `loom run` handoff, and a caller-provided dispatch
//! closure for the reviewer agent invocation. The closure pattern keeps
//! backend selection (`PiBackend` vs `ClaudeBackend`) inside the binary's
//! `dispatch` match — `loom-workflow` never sees the concrete backend types,
//! mirroring [`ProductionTodoController`](super::super::todo::ProductionTodoController)
//! and [`ProductionAgentLoopController`](super::super::run::ProductionAgentLoopController).
//!
//! Iteration-counter accessors read/write `molecules.iteration_count` for
//! the active molecule of `self.label`. `iteration_count` returns 0 when no
//! molecule has been seeded yet (the auto-iterate gate treats this as the
//! start of a cycle); `set_iteration_count` errors loudly if the active
//! molecule is missing so a misconfigured run cannot loop forever; `reset`
//! is a no-op in that case so the Clean push path is unaffected on a
//! freshly-init'd workspace.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use loom_core::agent::{ProtocolError, RePinContent, SessionOutcome, SpawnConfig};
use loom_core::bd::{BdClient, Bead, ListOpts, UpdateOpts};
use loom_core::git::GitClient;
use loom_core::identifier::{BeadId, ProfileName, SpecLabel};
use loom_core::lock::LockGuard;
use loom_core::profile_manifest::ProfileImageManifest;
use loom_core::state::StateDb;
use loom_templates::check::CheckContext;
use tokio::process::Command;
use tracing::info;

use super::context::beads_summary;
use super::error::CheckError;
use super::runner::{CheckController, ReviewOutcome};

pub struct ProductionCheckController<S, F>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<Output = Result<SessionOutcome, ProtocolError>> + Send,
{
    bd: BdClient,
    label: SpecLabel,
    loom_bin: PathBuf,
    workspace: PathBuf,
    state: Arc<StateDb>,
    manifest: Arc<ProfileImageManifest>,
    phase_default: ProfileName,
    spawn: S,
    /// Spec lock dropped before exec'ing `loom run` so the child can take it.
    lock: Option<LockGuard>,
}

impl<S, F> ProductionCheckController<S, F>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<Output = Result<SessionOutcome, ProtocolError>> + Send,
{
    #[expect(clippy::too_many_arguments, reason = "controller construction surface")]
    pub fn new(
        bd: BdClient,
        label: SpecLabel,
        loom_bin: PathBuf,
        workspace: PathBuf,
        state: Arc<StateDb>,
        manifest: Arc<ProfileImageManifest>,
        phase_default: ProfileName,
        spawn: S,
    ) -> Self {
        Self {
            bd,
            label,
            loom_bin,
            workspace,
            state,
            manifest,
            phase_default,
            spawn,
            lock: None,
        }
    }

    /// Hand the spec lock to the controller so `exec_run` can drop it
    /// before spawning the `loom run` child (which acquires the same lock).
    pub fn with_handoff_lock(mut self, guard: LockGuard) -> Self {
        self.lock = Some(guard);
        self
    }

    fn spec_label_filter(&self) -> String {
        format!("spec:{}", self.label.as_str())
    }

    /// Push gate must invoke `beads-push`, NOT `bd dolt push` — only
    /// `beads-push` syncs the `beads` git branch to GitHub.
    fn beads_push_command(&self) -> Command {
        let mut cmd = Command::new("beads-push");
        cmd.current_dir(&self.workspace);
        cmd
    }

    async fn build_review_prompt(&self) -> Result<String, CheckError> {
        let beads = self
            .bd
            .list(ListOpts {
                status: None,
                label: Some(self.spec_label_filter()),
            })
            .await?;
        let active_mol = self.state.active_molecule(&self.label)?;
        let molecule_id = active_mol.as_ref().map(|m| m.id.clone());
        let base_commit = active_mol.and_then(|m| m.base_commit);
        let ctx = CheckContext {
            pinned_context: String::new(),
            label: self.label.clone(),
            spec_path: format!("specs/{}.md", self.label.as_str()),
            companion_paths: vec![],
            beads_summary: beads_summary(&beads),
            base_commit,
            molecule_id,
            exit_signals: String::new(),
        };
        Ok(ctx.render()?)
    }
}

impl<S, F> CheckController for ProductionCheckController<S, F>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<Output = Result<SessionOutcome, ProtocolError>> + Send,
{
    async fn run_review(&mut self) -> Result<ReviewOutcome, CheckError> {
        let prompt = self.build_review_prompt().await?;
        let entry = self.manifest.lookup(&self.phase_default)?;
        let banner = format!("loom check @ {}", self.label);
        let scratch = loom_core::scratch::ScratchSession::open(
            &self.workspace,
            self.label.as_str(),
            &prompt,
            &banner,
        )
        .map_err(|source| CheckError::Protocol(ProtocolError::Io(source)))?;
        let spawn_config = SpawnConfig {
            image_ref: entry.r#ref.clone(),
            image_source: entry.source.clone(),
            workspace: self.workspace.clone(),
            env: vec![],
            initial_prompt: prompt,
            agent_args: vec![],
            repin: RePinContent {
                orientation: banner,
                pinned_context: String::new(),
                partial_bodies: vec![],
            },
            scratch_dir: scratch.path().to_path_buf(),
            model: None,
            shutdown_grace: None,
            handshake_timeout: None,
            stall_warn_interval: None,
        };
        info!(
            label = %self.label,
            image_ref = %spawn_config.image_ref,
            "loom check: dispatching reviewer agent",
        );
        let outcome = (self.spawn)(spawn_config).await;
        drop(scratch);
        let outcome = outcome?;
        if outcome.exit_code == 0 {
            Ok(ReviewOutcome::Complete)
        } else {
            Ok(ReviewOutcome::Incomplete {
                detail: format!("agent exited with code {}", outcome.exit_code),
            })
        }
    }

    async fn list_spec_beads(&mut self) -> Result<Vec<Bead>, CheckError> {
        let beads = self
            .bd
            .list(ListOpts {
                status: None,
                label: Some(self.spec_label_filter()),
            })
            .await?;
        Ok(beads)
    }

    async fn iteration_count(&mut self) -> Result<u32, CheckError> {
        Ok(self
            .state
            .active_molecule(&self.label)?
            .map(|m| m.iteration_count)
            .unwrap_or(0))
    }

    async fn set_iteration_count(&mut self, next: u32) -> Result<(), CheckError> {
        let mol = self
            .state
            .active_molecule(&self.label)?
            .ok_or_else(|| CheckError::NoActiveMolecule(self.label.to_string()))?;
        self.state.set_iteration(&mol.id, next)?;
        Ok(())
    }

    async fn reset_iteration_count(&mut self) -> Result<(), CheckError> {
        if let Some(mol) = self.state.active_molecule(&self.label)? {
            self.state.reset_iteration(&mol.id)?;
        }
        Ok(())
    }

    async fn apply_clarify(&mut self, bead: &BeadId, _reason: &str) -> Result<(), CheckError> {
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

    async fn git_push(&mut self) -> Result<(), CheckError> {
        let client = GitClient::open(&self.workspace)
            .map_err(|e| CheckError::GitPushFailed(e.to_string()))?;
        client
            .push()
            .await
            .map_err(|e| CheckError::GitPushFailed(e.to_string()))?;
        Ok(())
    }

    async fn beads_push(&mut self) -> Result<(), CheckError> {
        let output = self.beads_push_command().output().await?;
        if !output.status.success() {
            return Err(CheckError::BeadsPushFailed(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(())
    }

    async fn exec_run(&mut self) -> Result<(), CheckError> {
        // Release the spec lock before spawning the child — `loom run`
        // acquires the same lock and would otherwise time out behind us.
        self.lock.take();
        let status = Command::new(&self.loom_bin)
            .current_dir(&self.workspace)
            .arg("run")
            .arg("-s")
            .arg(self.label.as_str())
            .status()
            .await?;
        if !status.success() {
            return Err(CheckError::RunHandoff(status.to_string()));
        }
        Ok(())
    }
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
    use crate::check::runner::CheckController;
    use loom_core::identifier::MoleculeId;
    use loom_core::state::ActiveMolecule;
    use std::ffi::OsStr;
    use std::future::Ready;

    type NoopSpawn = fn(SpawnConfig) -> Ready<Result<SessionOutcome, ProtocolError>>;

    fn noop_spawn(_cfg: SpawnConfig) -> Ready<Result<SessionOutcome, ProtocolError>> {
        std::future::ready(Ok(SessionOutcome {
            exit_code: 0,
            cost_usd: None,
        }))
    }

    fn stub_manifest(dir: &std::path::Path) -> Arc<ProfileImageManifest> {
        let body = r#"{
          "base": { "ref": "localhost/wrapix-base:abc", "source": "/nix/store/aaa-image-base" }
        }"#;
        let path = dir.join("profile-images.json");
        std::fs::write(&path, body).unwrap();
        Arc::new(ProfileImageManifest::from_path(&path).unwrap())
    }

    fn empty_state(workspace: &std::path::Path) -> Arc<StateDb> {
        Arc::new(StateDb::open(workspace.join(".wrapix/loom/state.db")).unwrap())
    }

    fn seeded_state(workspace: &std::path::Path, label: &str, mol: &str) -> Arc<StateDb> {
        std::fs::create_dir_all(workspace.join("specs")).unwrap();
        std::fs::write(
            workspace.join(format!("specs/{label}.md")),
            format!("# {label}\n"),
        )
        .unwrap();
        let db = StateDb::open(workspace.join(".wrapix/loom/state.db")).unwrap();
        db.rebuild(
            workspace,
            &[ActiveMolecule {
                id: MoleculeId::new(mol),
                spec_label: SpecLabel::new(label),
                base_commit: None,
            }],
        )
        .unwrap();
        Arc::new(db)
    }

    fn controller(
        workspace: PathBuf,
    ) -> ProductionCheckController<NoopSpawn, Ready<Result<SessionOutcome, ProtocolError>>> {
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        ProductionCheckController::new(
            BdClient::new(),
            SpecLabel::new("loom-harness"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            noop_spawn,
        )
    }

    fn controller_with_state(
        workspace: PathBuf,
        label: &str,
        state: Arc<StateDb>,
    ) -> ProductionCheckController<NoopSpawn, Ready<Result<SessionOutcome, ProtocolError>>> {
        let manifest = stub_manifest(&workspace);
        ProductionCheckController::new(
            BdClient::new(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            noop_spawn,
        )
    }

    #[test]
    fn beads_push_argv_invokes_beads_push_not_bd_dolt_push() {
        let dir = tempfile::tempdir().unwrap();
        let ctrl = controller(dir.path().to_path_buf());
        let cmd = ctrl.beads_push_command();
        let std_cmd = cmd.as_std();

        assert_eq!(
            std_cmd.get_program(),
            OsStr::new("beads-push"),
            "push gate must shell out to beads-push, not bd",
        );
        let argv: Vec<&OsStr> = std_cmd.get_args().collect();
        assert!(
            argv.is_empty(),
            "no extra args; `bd dolt push` would surface as program=bd args=[dolt, push]: argv={argv:?}",
        );
        assert_eq!(std_cmd.get_current_dir(), Some(dir.path()));
    }

    #[tokio::test]
    async fn iteration_counter_round_trips_through_state_db() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let state = seeded_state(workspace, "alpha", "wx-alpha");
        let mut ctrl = controller_with_state(workspace.to_path_buf(), "alpha", state);

        assert_eq!(ctrl.iteration_count().await.unwrap(), 0);

        ctrl.set_iteration_count(3).await.unwrap();
        assert_eq!(ctrl.iteration_count().await.unwrap(), 3);

        ctrl.reset_iteration_count().await.unwrap();
        assert_eq!(ctrl.iteration_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn iteration_count_is_zero_when_no_active_molecule() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctrl = controller(dir.path().to_path_buf());
        assert_eq!(ctrl.iteration_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn set_iteration_errors_when_no_active_molecule() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctrl = controller(dir.path().to_path_buf());
        let err = ctrl.set_iteration_count(1).await.unwrap_err();
        assert!(
            matches!(err, CheckError::NoActiveMolecule(ref s) if s == "loom-harness"),
            "expected NoActiveMolecule(loom-harness), got {err:?}",
        );
    }

    #[tokio::test]
    async fn reset_iteration_is_no_op_when_no_active_molecule() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctrl = controller(dir.path().to_path_buf());
        ctrl.reset_iteration_count().await.unwrap();
    }

    #[tokio::test]
    async fn run_review_translates_zero_exit_into_complete() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let mut ctrl = ProductionCheckController::new(
            BdClient::new(),
            SpecLabel::new("loom-harness"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            |_cfg: SpawnConfig| async move {
                Ok(SessionOutcome {
                    exit_code: 0,
                    cost_usd: None,
                })
            },
        );
        // build_review_prompt calls bd; we bypass that path by stubbing the
        // BdClient through the live `bd` binary on the host (tests in this
        // crate use BdClient::new() identically). Skip if bd is unavailable.
        let outcome = ctrl.run_review().await;
        if let Err(CheckError::Bd(_)) = outcome {
            return;
        }
        assert!(
            matches!(outcome, Ok(ReviewOutcome::Complete)),
            "expected Complete, got {outcome:?}",
        );
    }

    #[tokio::test]
    async fn run_review_translates_nonzero_exit_into_incomplete_with_code() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let mut ctrl = ProductionCheckController::new(
            BdClient::new(),
            SpecLabel::new("loom-harness"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            |_cfg: SpawnConfig| async move {
                Ok(SessionOutcome {
                    exit_code: 7,
                    cost_usd: None,
                })
            },
        );
        let outcome = ctrl.run_review().await;
        if let Err(CheckError::Bd(_)) = outcome {
            return;
        }
        match outcome {
            Ok(ReviewOutcome::Incomplete { detail }) => {
                assert!(
                    detail.contains('7'),
                    "detail should mention exit 7: {detail}"
                );
            }
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }

    /// Regression: `exec_run` (the check → run handoff for auto-iterate)
    /// must release the spec lock before spawning, so the `loom run` child
    /// can acquire it. Mirror of the run-side test in `run/production.rs`.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_run_releases_lock_before_spawning_child() {
        use loom_core::clock::SystemClock;
        use loom_core::lock::LockManager;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let mgr = LockManager::new(&workspace).unwrap();
        let label = SpecLabel::new("alpha");
        let clock = SystemClock::new();
        let guard = mgr.acquire_spec_async(&label, &clock).await.unwrap();

        // Stand-in for the `loom` binary; /bin/true is absent on NixOS.
        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut ctrl = ProductionCheckController::new(
            BdClient::new(),
            label.clone(),
            stub,
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            |_cfg: SpawnConfig| async move {
                Ok(SessionOutcome {
                    exit_code: 0,
                    cost_usd: None,
                })
            },
        )
        .with_handoff_lock(guard);

        ctrl.exec_run().await.expect("exec_run ok");

        let _reacquired = mgr
            .acquire_spec_with_timeout_async(&label, &clock, Duration::from_millis(100))
            .await
            .expect("lock must be reacquirable after exec_run");
    }
}
