//! Production [`TodoController`] used by the `loom todo` binary.
//!
//! Resolves the per-bead [`SpawnConfig`] by running the four-tier detection
//! against a real [`GitClient`] + [`StateDb`], rendering `todo_new.md` /
//! `todo_update.md` from `loom-templates`, and persisting a per-spec
//! `todo_cursor` on success so subsequent tier-1 runs diff from the latest
//! anchor instead of the molecule's original `base_commit`.
//!
//! Agent dispatch happens in [`super::runner::run`] via a caller-provided
//! closure, so this controller does not own the spawn surface.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use loom_core::agent::{RePinContent, SessionOutcome, SpawnConfig};
use loom_core::git::GitClient;
use loom_core::identifier::{ProfileName, SpecLabel};
use loom_core::profile_manifest::ProfileImageManifest;
use loom_core::state::StateDb;
use tracing::{debug, info, warn};

use super::context::{TemplateBaseFields, TodoTemplateContext, build_template_context};
use super::error::TodoError;
use super::runner::TodoController;
use super::tier::{GitDiffSource, MoleculeState, TierInputs, compute_spec_diff};

pub struct ProductionTodoController {
    label: SpecLabel,
    workspace: PathBuf,
    state: Arc<StateDb>,
    manifest: Arc<ProfileImageManifest>,
    phase_default: ProfileName,
    git: Arc<GitClient>,
    since: Option<String>,
}

impl ProductionTodoController {
    pub fn new(
        label: SpecLabel,
        workspace: PathBuf,
        state: Arc<StateDb>,
        manifest: Arc<ProfileImageManifest>,
        phase_default: ProfileName,
        git: Arc<GitClient>,
        since: Option<String>,
    ) -> Self {
        Self {
            label,
            workspace,
            state,
            manifest,
            phase_default,
            git,
            since,
        }
    }

    fn build_prompt(&self) -> Result<String, TodoError> {
        let active_mol = self.state.active_molecule(&self.label)?;
        let molecule_id = active_mol.as_ref().map(|m| m.id.clone());

        // Layer the per-spec todo cursor over the molecule's stored
        // `base_commit`: the cursor moves forward after every successful
        // todo run so subsequent tier-1 diffs only show changes since then.
        // The molecule's original `base_commit` is kept as the seed when no
        // cursor has been recorded yet.
        let cursor = self.state.todo_cursor(&self.label)?;
        let molecule = active_mol.as_ref().map(|row| MoleculeState {
            id: row.id.clone(),
            base_commit: cursor.clone().or_else(|| row.base_commit.clone()),
        });

        let spec_path = PathBuf::from("specs").join(format!("{}.md", self.label.as_str()));
        let sibling_base = |label: &SpecLabel| -> Option<String> {
            self.state
                .active_molecule(label)
                .ok()
                .flatten()
                .and_then(|m| m.base_commit)
        };

        let inputs = TierInputs {
            label: &self.label,
            spec_path: &spec_path,
            molecule,
            since: self.since.as_deref(),
            sibling_base: &sibling_base,
        };

        let live_git = LiveGitDiffSource(Arc::clone(&self.git));
        let tier = compute_spec_diff(&live_git, &inputs)?;
        debug!(label = %self.label, ?tier, "tier decision");

        let base = TemplateBaseFields {
            label: self.label.clone(),
            spec_path: spec_path.to_string_lossy().into_owned(),
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
            shutdown_grace: None,
        })
    }

    async fn record_outcome(&mut self, outcome: &SessionOutcome) -> Result<(), TodoError> {
        if outcome.exit_code != 0 {
            info!(
                label = %self.label,
                exit_code = outcome.exit_code,
                "loom todo: nonzero exit — cursor not advanced",
            );
            return Ok(());
        }
        match self.git.head_commit_sha().await {
            Ok(head) => {
                self.state.set_todo_cursor(&self.label, &head)?;
                info!(
                    label = %self.label,
                    head = %head,
                    "loom todo: cursor advanced to HEAD",
                );
            }
            Err(e) => {
                warn!(
                    label = %self.label,
                    error = %e,
                    "loom todo: could not resolve HEAD — cursor unchanged",
                );
            }
        }
        Ok(())
    }
}

/// Bridge `compute_spec_diff`'s sync [`GitDiffSource`] surface to the async
/// [`GitClient`]. Each method runs the underlying git CLI on the current
/// tokio runtime via `block_in_place` + `Handle::current().block_on(...)`,
/// which is sound only on multi-thread runtimes — production wires this
/// from `loom`'s `Runtime::new()` (multi-thread by default), and tier-1
/// tests in this module that exercise it use
/// `#[tokio::test(flavor = "multi_thread")]`.
///
/// Errors are mapped to algorithm-safe defaults (`false` / empty) so a
/// transient git failure degrades to "no tier-1 diff" rather than aborting
/// the run; the user still sees a tier-2/4 prompt and can retry.
struct LiveGitDiffSource(Arc<GitClient>);

impl GitDiffSource for LiveGitDiffSource {
    fn rev_exists(&self, rev: &str) -> bool {
        let client = Arc::clone(&self.0);
        let rev = rev.to_string();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async move { client.rev_exists(&rev).await.unwrap_or(false) })
        })
    }

    fn is_ancestor_of_head(&self, rev: &str) -> bool {
        let client = Arc::clone(&self.0);
        let rev = rev.to_string();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async move { client.is_ancestor_of_head(&rev).await.unwrap_or(false) })
        })
    }

    fn changed_spec_files(&self, base: &str) -> Vec<std::path::PathBuf> {
        let client = Arc::clone(&self.0);
        let base = base.to_string();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async move { client.changed_spec_files(&base).await.unwrap_or_default() })
        })
    }

    fn diff_spec(&self, base: &str, spec_path: &std::path::Path) -> String {
        let client = Arc::clone(&self.0);
        let base = base.to_string();
        let spec_path = spec_path.to_path_buf();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                client
                    .diff_spec(&base, &spec_path)
                    .await
                    .unwrap_or_default()
            })
        })
    }
}
