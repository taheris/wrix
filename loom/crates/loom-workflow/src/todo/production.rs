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

use super::ExitSignal;
use super::context::{TemplateBaseFields, TodoTemplateContext, build_template_context};
use super::error::TodoError;
use super::runner::{TodoController, TodoSession};
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
        // Notes carry transient implementer hints from the most recent
        // `loom plan`. Missing-row is tolerated: the spec may not have been
        // through `plan` yet (tier-4 first-touch), in which case there are
        // simply no notes to render.
        let implementation_notes = match self.state.spec(&self.label) {
            Ok(row) => row.implementation_notes.unwrap_or_default(),
            Err(loom_core::state::StateError::SpecNotFound { .. }) => Vec::new(),
            Err(e) => return Err(TodoError::State(e)),
        };

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
            implementation_notes,
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
    async fn build_session(&mut self) -> Result<TodoSession, TodoError> {
        let prompt = self.build_prompt()?;
        let entry = self.manifest.lookup(&self.phase_default)?;
        let banner = format!("loom todo @ {}", self.label);
        let scratch = loom_core::scratch::ScratchSession::open(
            &self.workspace,
            self.label.as_str(),
            &prompt,
            &banner,
        )
        .map_err(|source| TodoError::Protocol(loom_core::agent::ProtocolError::Io(source)))?;
        info!(
            label = %self.label,
            workspace = %self.workspace.display(),
            image_ref = %entry.r#ref,
            scratch_dir = %scratch.path().display(),
            "loom todo: building spawn config",
        );
        let scratch_dir = scratch.path().to_path_buf();
        Ok(TodoSession {
            config: SpawnConfig {
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
                scratch_dir,
                model: None,
                shutdown_grace: None,
                handshake_timeout: None,
                stall_warn_interval: None,
            },
            scratch,
        })
    }

    async fn record_outcome(
        &mut self,
        outcome: &SessionOutcome,
        marker: Option<&ExitSignal>,
    ) -> Result<(), TodoError> {
        if !cursor_should_advance(outcome.exit_code, marker) {
            info!(
                label = %self.label,
                exit_code = outcome.exit_code,
                marker = ?marker,
                "loom todo: cursor not advanced — gate requires exit_code==0 AND LOOM_COMPLETE/LOOM_NOOP",
            );
            return Ok(());
        }
        match self.git.head_commit_sha().await {
            Ok(head) => {
                self.state.set_todo_cursor(&self.label, &head)?;
                info!(
                    label = %self.label,
                    head = %head,
                    marker = ?marker,
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
        // Same gate as the cursor: notes are transient implementer hints
        // consumed once into the bead body and only meaningfully "consumed"
        // when the agent reported productive completion. Tolerate the
        // missing-row case because tier-4 first-touch may not have a row
        // yet (no `loom plan` run before this `loom todo`).
        match self.state.clear_implementation_notes(&self.label) {
            Ok(()) => {
                info!(
                    label = %self.label,
                    "loom todo: implementation_notes cleared after consume",
                );
            }
            Err(loom_core::state::StateError::SpecNotFound { .. }) => {}
            Err(e) => return Err(TodoError::State(e)),
        }
        Ok(())
    }
}

/// Cursor-advance gate per `specs/loom-harness.md` lines 902-918: both
/// `exit_code == 0` and a `LOOM_COMPLETE`/`LOOM_NOOP` marker required.
fn cursor_should_advance(exit_code: i32, marker: Option<&ExitSignal>) -> bool {
    exit_code == 0 && matches!(marker, Some(ExitSignal::Complete | ExitSignal::Noop))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Five terminal marker shapes × two exit codes — ten rows, two
    /// truths: only `LOOM_COMPLETE`/`LOOM_NOOP` paired with `exit_code==0`
    /// advances the cursor (`specs/loom-harness.md` lines 902-918).
    #[test]
    fn cursor_gate_advances_only_on_complete_or_noop_with_clean_exit() {
        let blocked = ExitSignal::Blocked {
            reason: "missing schema".into(),
        };
        let clarify = ExitSignal::Clarify {
            question: "additive only?".into(),
        };
        let cases: &[(Option<&ExitSignal>, i32, bool, &str)] = &[
            // Clean exit: only Complete and Noop advance the cursor.
            (Some(&ExitSignal::Complete), 0, true, "complete + exit 0"),
            (Some(&ExitSignal::Noop), 0, true, "noop + exit 0"),
            (Some(&blocked), 0, false, "blocked + exit 0"),
            (Some(&clarify), 0, false, "clarify + exit 0"),
            (None, 0, false, "no marker + exit 0"),
            // Nonzero exit: gate refuses regardless of marker — covers
            // the swallowed-marker and backend-error paths called out in
            // the spec (529 overload, network drop, watchdog timeout).
            (Some(&ExitSignal::Complete), 1, false, "complete + exit 1"),
            (Some(&ExitSignal::Noop), 1, false, "noop + exit 1"),
            (Some(&blocked), 1, false, "blocked + exit 1"),
            (Some(&clarify), 1, false, "clarify + exit 1"),
            (None, 1, false, "no marker + exit 1"),
        ];
        for (marker, exit_code, expected, label) in cases {
            assert_eq!(
                cursor_should_advance(*exit_code, *marker),
                *expected,
                "case `{label}`: expected advance={expected}",
            );
        }
    }
}
