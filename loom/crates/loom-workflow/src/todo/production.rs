//! Production [`TodoController`] used by the `loom todo` binary.
//!
//! Builds a stub [`SpawnConfig`] and a no-op `record_outcome` so the
//! binary's `loom todo` path can wire through `dispatch` without the
//! decomposition agent being implemented yet (the four-tier detection logic
//! is exercised by `loom-workflow::todo::tier::tests`). The controller will
//! gain real spec-loading, prompt-rendering, and per-spec cursor wiring as
//! the surrounding tasks (wx-3hhwq.26 and follow-ups) land.

use std::path::PathBuf;

use loom_core::agent::{RePinContent, SessionOutcome, SpawnConfig};
use loom_core::identifier::SpecLabel;
use tracing::info;

use super::error::TodoError;
use super::runner::TodoController;

/// Minimal production controller. Subsequent issues will replace the stubs
/// here with real spec ingestion and per-spec cursor persistence — the
/// signature is fixed so the binary's `dispatch` wiring stays stable.
pub struct ProductionTodoController {
    label: SpecLabel,
    workspace: PathBuf,
    image_ref: String,
    image_source: PathBuf,
}

impl ProductionTodoController {
    pub fn new(
        label: SpecLabel,
        workspace: PathBuf,
        image_ref: String,
        image_source: PathBuf,
    ) -> Self {
        Self {
            label,
            workspace,
            image_ref,
            image_source,
        }
    }
}

impl TodoController for ProductionTodoController {
    async fn build_spawn_config(&mut self) -> Result<SpawnConfig, TodoError> {
        info!(
            label = %self.label,
            workspace = %self.workspace.display(),
            image_ref = %self.image_ref,
            image_source = %self.image_source.display(),
            "loom todo: building stub spawn config (decomposition agent pending)",
        );
        Ok(SpawnConfig {
            image_ref: self.image_ref.clone(),
            image_source: self.image_source.clone(),
            workspace: self.workspace.clone(),
            env: vec![],
            initial_prompt: format!("loom todo: decompose spec {}", self.label.as_str()),
            agent_args: vec![],
            repin: RePinContent {
                orientation: String::new(),
                pinned_context: String::new(),
                partial_bodies: vec![],
            },
            model: None,
        })
    }

    async fn record_outcome(&mut self, outcome: &SessionOutcome) -> Result<(), TodoError> {
        info!(
            label = %self.label,
            exit_code = outcome.exit_code,
            cost_usd = ?outcome.cost_usd,
            "loom todo: outcome recorded (cursor persistence pending)",
        );
        Ok(())
    }
}
