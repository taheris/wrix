//! Production [`TodoController`] used by the `loom todo` binary.
//!
//! Builds a real [`SpawnConfig`] by looking up the profile image via
//! [`ProfileImageManifest`] and rendering `todo_new.md` / `todo_update.md`
//! from `loom-templates`. Tier selection is currently limited to tiers 2 + 4
//! (driven by [`StateDb::active_molecule`]) — the tier-1 git diff path and
//! per-spec cursor persistence are tracked under wx-9z0nq, which adds a
//! `GitDiffSource` adapter on `GitClient` and a typed cursor accessor on
//! `StateDb`.
//!
//! `record_outcome` is a no-op until that follow-up lands. Agent dispatch
//! happens in [`super::runner::run`] via a caller-provided closure, so this
//! controller does not own the spawn surface.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use loom_core::agent::{RePinContent, SessionOutcome, SpawnConfig};
use loom_core::identifier::{ProfileName, SpecLabel};
use loom_core::profile_manifest::ProfileImageManifest;
use loom_core::state::StateDb;
use tracing::info;

use super::context::{TemplateBaseFields, TodoTemplateContext, build_template_context};
use super::error::TodoError;
use super::runner::TodoController;
use super::tier::TierDecision;

pub struct ProductionTodoController {
    label: SpecLabel,
    workspace: PathBuf,
    state: Arc<StateDb>,
    manifest: Arc<ProfileImageManifest>,
    phase_default: ProfileName,
}

impl ProductionTodoController {
    pub fn new(
        label: SpecLabel,
        workspace: PathBuf,
        state: Arc<StateDb>,
        manifest: Arc<ProfileImageManifest>,
        phase_default: ProfileName,
    ) -> Self {
        Self {
            label,
            workspace,
            state,
            manifest,
            phase_default,
        }
    }

    fn build_prompt(&self) -> Result<String, TodoError> {
        let active_mol = self.state.active_molecule(&self.label)?;
        let molecule_id = active_mol.as_ref().map(|m| m.id.clone());
        // Tier 2 when a molecule already exists, tier 4 otherwise. Tier 1
        // (git-diff fan-out) requires a `GitDiffSource` adapter that does not
        // yet exist on `GitClient` — tracked in wx-9z0nq.
        let tier = match &molecule_id {
            Some(id) => TierDecision::Tasks {
                molecule: id.clone(),
            },
            None => TierDecision::New,
        };
        let base = TemplateBaseFields {
            label: self.label.clone(),
            spec_path: format!("specs/{}.md", self.label.as_str()),
            pinned_context: String::new(),
            companion_paths: vec![],
            implementation_notes: vec![],
            exit_signals: String::new(),
        };
        let ctx = build_template_context(&tier, base, None, molecule_id);
        let body = match ctx {
            TodoTemplateContext::New(c) => c.render()?,
            TodoTemplateContext::Update(c) => c.render()?,
        };
        Ok(body)
    }
}

impl TodoController for ProductionTodoController {
    async fn build_spawn_config(&mut self) -> Result<SpawnConfig, TodoError> {
        let prompt = self.build_prompt()?;
        let entry = self.manifest.lookup(&self.phase_default)?;
        info!(
            label = %self.label,
            workspace = %self.workspace.display(),
            image_ref = %entry.r#ref,
            "loom todo: building spawn config",
        );
        Ok(SpawnConfig {
            image_ref: entry.r#ref.clone(),
            image_source: entry.source.clone(),
            workspace: self.workspace.clone(),
            env: vec![],
            initial_prompt: prompt,
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
            "loom todo: outcome recorded (cursor persistence pending — wx-9z0nq)",
        );
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
    use loom_core::identifier::MoleculeId;
    use loom_core::state::ActiveMolecule;

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

    #[tokio::test]
    async fn build_spawn_config_resolves_manifest_image_and_renders_new_template() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let mut ctrl = ProductionTodoController::new(
            SpecLabel::new("alpha"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
        );
        let cfg = ctrl.build_spawn_config().await.expect("build cfg");
        assert_eq!(cfg.image_ref, "localhost/wrapix-base:abc");
        assert_eq!(
            cfg.image_source,
            std::path::PathBuf::from("/nix/store/aaa-image-base"),
        );
        assert!(
            cfg.initial_prompt.contains("Task Decomposition"),
            "TodoNewContext renders todo_new.md (header marker missing): {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.initial_prompt.contains("alpha"),
            "spec label must appear in rendered prompt: {}",
            cfg.initial_prompt,
        );
    }

    #[tokio::test]
    async fn build_spawn_config_uses_update_template_when_molecule_exists() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let state = seeded_state(&workspace, "alpha", "wx-mol");
        let manifest = stub_manifest(&workspace);
        let mut ctrl = ProductionTodoController::new(
            SpecLabel::new("alpha"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
        );
        let cfg = ctrl.build_spawn_config().await.expect("build cfg");
        assert!(
            cfg.initial_prompt.contains("wx-mol"),
            "molecule id must thread into update template: {}",
            cfg.initial_prompt,
        );
    }

    #[tokio::test]
    async fn build_spawn_config_surfaces_unknown_profile_as_profile_error() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let mut ctrl = ProductionTodoController::new(
            SpecLabel::new("alpha"),
            workspace,
            state,
            manifest,
            ProfileName::new("missing"),
        );
        let err = ctrl
            .build_spawn_config()
            .await
            .expect_err("missing profile");
        assert!(
            matches!(err, TodoError::Profile(_)),
            "expected Profile, got {err:?}",
        );
    }
}
