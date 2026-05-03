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
//! The iteration counter accessors are stubbed at zero — the real wiring
//! reads/writes `molecules.iteration_count` for the active molecule, and
//! lands together with the agent backend so the auto-iterate path can
//! actually exercise it.

use std::path::PathBuf;

use loom_core::bd::{BdClient, Bead, ListOpts, UpdateOpts};
use loom_core::identifier::{BeadId, SpecLabel};
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
}

impl ProductionCheckController {
    pub fn new(bd: BdClient, label: SpecLabel, loom_bin: PathBuf, workspace: PathBuf) -> Self {
        Self {
            bd,
            label,
            loom_bin,
            workspace,
        }
    }

    fn spec_label_filter(&self) -> String {
        format!("spec:{}", self.label.as_str())
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
        Ok(0)
    }

    async fn set_iteration_count(&mut self, _next: u32) -> Result<(), CheckError> {
        Ok(())
    }

    async fn reset_iteration_count(&mut self) -> Result<(), CheckError> {
        Ok(())
    }

    async fn apply_clarify(&mut self, bead: &BeadId, _reason: &str) -> Result<(), CheckError> {
        self.bd
            .update(
                bead,
                UpdateOpts {
                    add_labels: vec!["ralph:clarify".to_string()],
                    ..UpdateOpts::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn git_push(&mut self) -> Result<(), CheckError> {
        let output = Command::new("git")
            .current_dir(&self.workspace)
            .arg("push")
            .output()
            .await?;
        if !output.status.success() {
            return Err(CheckError::GitPushFailed(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(())
    }

    async fn beads_push(&mut self) -> Result<(), CheckError> {
        let output = Command::new("beads-push")
            .current_dir(&self.workspace)
            .output()
            .await?;
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
