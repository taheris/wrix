//! Production [`CheckController`] used by the `loom check` binary.
//!
//! Wires `BdClient` for spec-bead snapshots and clarify, plus
//! `tokio::process::Command` shell-outs for `git push`, `beads-push`, and
//! the auto-iterate `loom run` handoff. The reviewer agent itself is
//! **not yet implemented** (`loom-agent` is a placeholder crate as of
//! wx-3hhwq.20); [`ProductionCheckController::run_review`] returns
//! [`ReviewOutcome::Incomplete`] so `check_loop` aborts before touching
//! the working tree.
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

use loom_core::bd::{BdClient, Bead, ListOpts, UpdateOpts};
use loom_core::git::GitClient;
use loom_core::identifier::{BeadId, SpecLabel};
use loom_core::state::StateDb;
use tokio::process::Command;
use tracing::warn;

use super::error::CheckError;
use super::runner::{CheckController, ReviewOutcome};

/// Stub detail returned by [`ProductionCheckController::run_review`] until
/// the `loom-agent` backend lands. `check_loop` surfaces this through
/// [`CheckError::ReviewIncomplete`].
pub const STUB_REVIEW_DETAIL: &str = "loom-agent backend not yet implemented (wx-3hhwq.20)";

pub struct ProductionCheckController {
    bd: BdClient,
    label: SpecLabel,
    loom_bin: PathBuf,
    workspace: PathBuf,
    state: Arc<StateDb>,
}

impl ProductionCheckController {
    pub fn new(
        bd: BdClient,
        label: SpecLabel,
        loom_bin: PathBuf,
        workspace: PathBuf,
        state: Arc<StateDb>,
    ) -> Self {
        Self {
            bd,
            label,
            loom_bin,
            workspace,
            state,
        }
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
}

impl CheckController for ProductionCheckController {
    async fn run_review(&mut self) -> Result<ReviewOutcome, CheckError> {
        warn!(
            label = %self.label,
            "loom check: agent backend stub — returning Incomplete (loom-agent crate is empty)",
        );
        Ok(ReviewOutcome::Incomplete {
            detail: STUB_REVIEW_DETAIL.to_string(),
        })
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
#[expect(clippy::unwrap_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use crate::check::runner::CheckController;
    use loom_core::identifier::MoleculeId;
    use loom_core::state::ActiveMolecule;
    use std::ffi::OsStr;

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

    fn controller(workspace: PathBuf) -> ProductionCheckController {
        let state = empty_state(&workspace);
        ProductionCheckController::new(
            BdClient::new(),
            SpecLabel::new("loom-harness"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
        )
    }

    fn controller_with_state(
        workspace: PathBuf,
        label: &str,
        state: Arc<StateDb>,
    ) -> ProductionCheckController {
        ProductionCheckController::new(
            BdClient::new(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
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
}
