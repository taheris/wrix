//! Production [`AgentLoopController`] used by the `loom run` binary.
//!
//! Wires `BdClient` for bead lookup/close/clarify and a `tokio::process::Command`
//! shell-out for `exec_check`. The agent backend itself is **not yet
//! implemented** (`loom-agent` is a placeholder crate as of wx-3hhwq.20), so
//! [`ProductionAgentLoopController::run_bead`] returns a stub failure that
//! the retry policy will surface as a clarify after exhausting attempts. The
//! `next_ready_bead → empty → exec_check` smoke path is fully wired and
//! exercised end-to-end.

use std::path::PathBuf;

use loom_core::bd::{BdClient, Bead, ListOpts, ReadyOpts, UpdateOpts};
use loom_core::identifier::{BeadId, SpecLabel};
use tokio::process::Command;
use tracing::info;

use super::error::RunError;
use super::outcome::AgentOutcome;
use super::runner::AgentLoopController;

/// Stub error body returned by [`ProductionAgentLoopController::run_bead`]
/// until the `loom-agent` backend lands. The retry policy surfaces this as a
/// clarify after the configured attempt count.
pub const STUB_AGENT_ERROR: &str = "loom-agent backend not yet implemented (wx-3hhwq.20)";

/// Wires the [`AgentLoopController`] trait against the real `BdClient` and a
/// child `loom check` exec for handoff.
pub struct ProductionAgentLoopController {
    bd: BdClient,
    label: SpecLabel,
    loom_bin: PathBuf,
    workspace: PathBuf,
}

impl ProductionAgentLoopController {
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

impl AgentLoopController for ProductionAgentLoopController {
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
        info!(
            bead = %bead.id,
            retry = previous_failure.is_some(),
            "loom run: agent backend stub — returning Failure (loom-agent crate is empty)",
        );
        Ok(AgentOutcome::Failure {
            error: STUB_AGENT_ERROR.to_string(),
        })
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
                    add_labels: vec!["ralph:clarify".to_string()],
                    ..UpdateOpts::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn exec_check(&mut self) -> Result<(), RunError> {
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
