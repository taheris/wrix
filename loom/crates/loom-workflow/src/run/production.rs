//! Production [`AgentLoopController`] used by the `loom run` binary.
//!
//! Wires `BdClient` for bead lookup/close/clarify and a `tokio::process::Command`
//! shell-out for `exec_check`. The agent backend itself is **not yet
//! implemented** (`loom-agent` is a placeholder crate as of wx-3hhwq.20), so
//! [`ProductionAgentLoopController::run_bead`] returns a stub failure that
//! the retry policy will surface as a clarify after exhausting attempts. The
//! `next_ready_bead → empty → exec_check` smoke path is fully wired and
//! exercised end-to-end.
//!
//! Per-bead profile dispatch is wired through [`build_spawn_config_from_manifest`]:
//! the manifest, CLI `--profile` override, and per-phase fallback all flow
//! into the controller at construction time so `run_bead` resolves the
//! per-bead `image_ref` + `image_source` against the parsed manifest before
//! the (stubbed) agent invocation. A missing manifest entry surfaces as
//! [`RunError::Profile`] — no silent fallback.

use std::path::PathBuf;
use std::sync::Arc;

use loom_core::agent::RePinContent;
use loom_core::bd::{BdClient, Bead, ListOpts, ReadyOpts, UpdateOpts};
use loom_core::identifier::{BeadId, ProfileName, SpecLabel};
use loom_core::profile_manifest::ProfileImageManifest;
use tokio::process::Command;
use tracing::info;

use super::error::RunError;
use super::outcome::AgentOutcome;
use super::runner::AgentLoopController;
use super::spawn::build_spawn_config_from_manifest;

/// Stub error body returned by [`ProductionAgentLoopController::run_bead`]
/// until the `loom-agent` backend lands. The retry policy surfaces this as a
/// clarify after the configured attempt count.
pub const STUB_AGENT_ERROR: &str = "loom-agent backend not yet implemented (wx-3hhwq.20)";

/// Wires the [`AgentLoopController`] trait against the real `BdClient` and a
/// child `loom check` exec for handoff.
///
/// `manifest` / `cli_profile` / `phase_default` are the inputs the per-bead
/// profile resolver chain needs (see
/// [`super::resolve_profile_image`]). They are stored on the controller so
/// every `run_bead` call resolves the bead's `image_ref` + `image_source`
/// from the same parsed manifest, never re-reading it from disk.
pub struct ProductionAgentLoopController {
    bd: BdClient,
    label: SpecLabel,
    loom_bin: PathBuf,
    workspace: PathBuf,
    manifest: Arc<ProfileImageManifest>,
    cli_profile: Option<ProfileName>,
    phase_default: ProfileName,
}

impl ProductionAgentLoopController {
    pub fn new(
        bd: BdClient,
        label: SpecLabel,
        loom_bin: PathBuf,
        workspace: PathBuf,
        manifest: Arc<ProfileImageManifest>,
        cli_profile: Option<ProfileName>,
        phase_default: ProfileName,
    ) -> Self {
        Self {
            bd,
            label,
            loom_bin,
            workspace,
            manifest,
            cli_profile,
            phase_default,
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
        // Resolve the per-bead profile image up front so a missing manifest
        // entry surfaces as a typed `RunError::Profile` (not as a stub
        // agent failure) before the stub ever runs. The constructed
        // SpawnConfig is otherwise unused until the loom-agent backend
        // lands; logging the resolved ref keeps the dispatch auditable.
        let spawn_config = build_spawn_config_from_manifest(
            &self.manifest,
            bead,
            self.cli_profile.as_ref(),
            &self.phase_default,
            self.workspace.clone(),
            format!("loom run: bead {}", bead.id),
            RePinContent {
                orientation: String::new(),
                pinned_context: String::new(),
                partial_bodies: vec![],
            },
            vec![],
            vec![],
        )?;
        info!(
            bead = %bead.id,
            image_ref = %spawn_config.image_ref,
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
                    add_labels: vec!["loom:clarify".to_string()],
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
