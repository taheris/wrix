//! Production [`ReviewController`] used by the `loom review` binary.
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
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use askama::Template;
use loom_driver::agent::{
    ProtocolError, RePinContent, SessionOutcome, SpawnConfig, set_loom_inside,
};
use loom_driver::bd::{BdClient, Bead, ListOpts, UpdateOpts};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::config::Phase;
use loom_driver::git::GitClient;
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::lock::LockGuard;
use loom_driver::logging::{BeadOutcome, LogSink};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::scratch::resolve_scratch_key;
use loom_driver::state::StateDb;
use loom_events::{AgentEvent, DriverKind, EnvelopeBuilder, Source};
use loom_templates::review::ReviewContext;
use tokio::process::Command;
use tracing::{info, warn};

use super::context::{beads_summary, load_review_sources};
use super::error::ReviewError;
use super::phase_verdict::{GateInputs, PhaseVerdict, RecoveryCause, decide};
use super::runner::{ReviewController, ReviewOutcome};
use crate::todo::ExitSignal;

pub struct ProductionReviewController<S, F>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<Output = Result<(SessionOutcome, Option<ExitSignal>), ProtocolError>>
        + Send,
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
    /// Phase log root + start timestamp. The verdict gate emits
    /// `push_gate_*` driver events into the same JSONL log file the
    /// reviewer agent writes to, so a replay can replay the full review
    /// phase. Both writers compute the file path from
    /// `(phase_log_root, label, "review", phase_log_when)`, which is
    /// deterministic — append-mode opens share one file.
    phase_log_root: Option<PathBuf>,
    phase_log_when: SystemTime,
    /// Per-phase envelope builder. The review phase isn't bead-scoped,
    /// so the envelope carries the synthetic `wx-review` bead id; the
    /// builder tracks `seq` across every `emit_driver_event` call so
    /// replay code can reorder events deterministically. Wrapped in
    /// `Mutex` because `EnvelopeBuilder`'s clock closure is `Send`
    /// but not `Sync` — the trait's `Send`-future bound requires the
    /// controller itself to be `Sync` across `&self` borrows.
    envelope_builder: Mutex<Option<EnvelopeBuilder>>,
    /// Workspace-relative path to the style-rules document pinned in the
    /// review prompt. Sourced from `LoomConfig.style_rules` at construction
    /// via [`Self::with_style_rules`]; defaults to the built-in path so
    /// test fakes that skip the builder still render a valid prompt.
    style_rules: String,
}

impl<S, F> ProductionReviewController<S, F>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<Output = Result<(SessionOutcome, Option<ExitSignal>), ProtocolError>>
        + Send,
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
            phase_log_root: None,
            phase_log_when: SystemClock::new().wall_now(),
            envelope_builder: Mutex::new(None),
            style_rules: "docs/style-rules.md".to_string(),
        }
    }

    /// Hand the spec lock to the controller so `exec_run` can drop it
    /// before spawning the `loom run` child (which acquires the same lock).
    pub fn with_handoff_lock(mut self, guard: LockGuard) -> Self {
        self.lock = Some(guard);
        self
    }

    /// Override the style-rules pin used in the rendered review prompt.
    /// Production callers thread this from `LoomConfig.style_rules`; tests
    /// rely on the built-in default.
    pub fn with_style_rules(mut self, path: String) -> Self {
        self.style_rules = path;
        self
    }

    /// Pin the phase log file the verdict gate's driver events stream
    /// into. The spawn closure inside `run_review` MUST use the same
    /// `when` when it opens its agent-event sink or the two writers
    /// land in separate files. Tests and the CLI share this via
    /// `phase_log_when()`.
    pub fn with_phase_log(mut self, logs_root: PathBuf, when: SystemTime) -> Self {
        self.phase_log_root = Some(logs_root);
        self.phase_log_when = when;
        self
    }

    /// The pinned phase log timestamp — read by the binary's spawn
    /// closure so its agent-event `LogSink` lands in the same file
    /// the controller's driver events append to.
    pub fn phase_log_when(&self) -> SystemTime {
        self.phase_log_when
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

    async fn build_review_prompt(&self) -> Result<String, ReviewError> {
        let beads = self
            .bd
            .list(ListOpts {
                status: None,
                label: Some(self.spec_label_filter()),
                ..ListOpts::default()
            })
            .await?;
        let active_mol = self.state.active_molecule(&self.label)?;
        let molecule_id = active_mol.as_ref().map(|m| m.id.clone());
        let base_commit = active_mol.and_then(|m| m.base_commit);
        let spec_path = format!("specs/{}.md", self.label.as_str());
        let (verify_sources, judge_rubrics) =
            load_review_sources(&self.workspace, &self.workspace.join(&spec_path))?;
        let key = resolve_scratch_key(Phase::Review, &self.label, None);
        let scratchpad_path =
            loom_driver::scratch::ScratchSession::scratchpad_path_for(&self.workspace, &key)
                .to_string_lossy()
                .into_owned();
        let ctx = ReviewContext {
            pinned_context: String::new(),
            label: self.label.clone(),
            spec_path,
            companion_paths: vec![],
            beads_summary: beads_summary(&beads),
            base_commit,
            molecule_id,
            verify_sources,
            judge_rubrics,
            scratchpad_path,
            style_rules: self.style_rules.clone(),
        };
        Ok(ctx.render()?)
    }
}

/// Map the reviewer agent's `(marker, exit_code)` into a [`ReviewOutcome`]
/// (FR12 — single source of truth). Marker → outcome routing goes through
/// the canonical [`decide`] gate function; the review phase isn't bead-
/// scoped, so `bd_closed` / `diff_empty` / verify / review observables are
/// neutral defaults that reduce the gate to marker-only routing. The
/// defensive `COMPLETE`/`NOOP` + non-zero exit guard predates `decide`
/// because the gate's decision table does not consider exit code.
fn classify_review_phase(marker: Option<&ExitSignal>, exit_code: i32) -> ReviewOutcome {
    if matches!(marker, Some(ExitSignal::Complete | ExitSignal::Noop)) && exit_code != 0 {
        return ReviewOutcome::Incomplete {
            detail: format!("agent emitted COMPLETE/NOOP but exited code {exit_code}"),
        };
    }
    let inputs = GateInputs {
        bd_closed: true,
        diff_empty: false,
        verify_failures: vec![],
        review_flag: None,
    };
    match decide(marker, inputs) {
        PhaseVerdict::Done => ReviewOutcome::Complete,
        PhaseVerdict::Blocked { reason } => ReviewOutcome::Incomplete {
            detail: format!("LOOM_BLOCKED: {reason}"),
        },
        PhaseVerdict::Clarify { question } => ReviewOutcome::Incomplete {
            detail: format!("LOOM_CLARIFY: {question}"),
        },
        PhaseVerdict::Recovery {
            cause: RecoveryCause::SwallowedMarker,
        } => ReviewOutcome::Incomplete {
            detail: if exit_code == 0 {
                "agent exited 0 without LOOM_COMPLETE / LOOM_BLOCKED / LOOM_CLARIFY marker \
                 (swallowed marker)"
                    .to_string()
            } else {
                format!("agent exited with code {exit_code}")
            },
        },
        PhaseVerdict::Recovery { cause } => ReviewOutcome::Incomplete {
            detail: format!("unexpected gate verdict: {}", cause.as_str()),
        },
    }
}

impl<S, F> ReviewController for ProductionReviewController<S, F>
where
    S: Fn(SpawnConfig) -> F + Send + Sync,
    F: std::future::Future<Output = Result<(SessionOutcome, Option<ExitSignal>), ProtocolError>>
        + Send,
{
    async fn run_review(&mut self) -> Result<ReviewOutcome, ReviewError> {
        let prompt = self.build_review_prompt().await?;
        let entry = self.manifest.lookup(&self.phase_default)?;
        let banner = format!("loom review @ {}", self.label);
        let key = resolve_scratch_key(Phase::Review, &self.label, None);
        let scratch =
            loom_driver::scratch::ScratchSession::open(&self.workspace, &key, &prompt, &banner)
                .map_err(|source| ReviewError::Protocol(ProtocolError::Io(source)))?;
        let mut env = Vec::new();
        set_loom_inside(&mut env);
        let spawn_config = SpawnConfig {
            image_ref: entry.r#ref.clone(),
            image_source: entry.source.clone(),
            workspace: self.workspace.clone(),
            env,
            initial_prompt: prompt,
            agent_args: vec![],
            repin: RePinContent {
                orientation: String::new(),
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
            "loom review: dispatching reviewer agent",
        );
        let result = (self.spawn)(spawn_config).await;
        drop(scratch);
        let (outcome, marker) = result?;
        Ok(classify_review_phase(marker.as_ref(), outcome.exit_code))
    }

    async fn list_spec_beads(&mut self) -> Result<Vec<Bead>, ReviewError> {
        let beads = self
            .bd
            .list(ListOpts {
                status: None,
                label: Some(self.spec_label_filter()),
                ..ListOpts::default()
            })
            .await?;
        Ok(beads)
    }

    async fn iteration_count(&mut self) -> Result<u32, ReviewError> {
        Ok(self
            .state
            .active_molecule(&self.label)?
            .map(|m| m.iteration_count)
            .unwrap_or(0))
    }

    async fn set_iteration_count(&mut self, next: u32) -> Result<(), ReviewError> {
        let mol = self
            .state
            .active_molecule(&self.label)?
            .ok_or_else(|| ReviewError::NoActiveMolecule(self.label.to_string()))?;
        self.state.set_iteration(&mol.id, next)?;
        Ok(())
    }

    async fn reset_iteration_count(&mut self) -> Result<(), ReviewError> {
        if let Some(mol) = self.state.active_molecule(&self.label)? {
            self.state.reset_iteration(&mol.id)?;
        }
        Ok(())
    }

    async fn apply_clarify(&mut self, bead: &BeadId, reason: &str) -> Result<(), ReviewError> {
        self.bd
            .update(
                bead,
                UpdateOpts {
                    add_labels: vec!["loom:clarify".to_string()],
                    notes: Some(reason.to_string()),
                    ..UpdateOpts::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn git_push(&mut self) -> Result<(), ReviewError> {
        let client = GitClient::open(&self.workspace)
            .map_err(|e| ReviewError::GitPushFailed(e.to_string()))?;
        client
            .push()
            .await
            .map_err(|e| ReviewError::GitPushFailed(e.to_string()))?;
        Ok(())
    }

    async fn beads_push(&mut self) -> Result<(), ReviewError> {
        let output = self.beads_push_command().output().await?;
        if !output.status.success() {
            return Err(ReviewError::BeadsPushFailed(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(())
    }

    async fn exec_run(&mut self) -> Result<(), ReviewError> {
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
            return Err(ReviewError::RunHandoff(status.to_string()));
        }
        Ok(())
    }

    fn emit_driver_event(&mut self, kind: DriverKind, summary: &str, payload: serde_json::Value) {
        // Open a transient LogSink at the same phase log path the
        // reviewer agent's sink uses (same `when`, same `phase_log_root`,
        // no renderer), write one `DriverEvent`, finish. The file is
        // opened in append mode so co-writing with the agent-event sink
        // lands both event streams in one file. When no phase log is
        // configured (test fakes, sink-less callers) this is a silent
        // no-op.
        let Some(logs_root) = self.phase_log_root.clone() else {
            return;
        };
        let mut guard = match self.envelope_builder.lock() {
            Ok(g) => g,
            Err(_) => {
                warn!("review controller: envelope builder mutex poisoned");
                return;
            }
        };
        if guard.is_none() {
            let synthetic_bead = match BeadId::new("wx-review") {
                Ok(id) => id,
                Err(e) => {
                    warn!(error = %e, "review controller: synthetic bead id invalid");
                    return;
                }
            };
            let clock = SystemClock::new();
            *guard = Some(EnvelopeBuilder::new(
                synthetic_bead,
                None,
                0,
                Source::Driver,
                move || {
                    clock
                        .wall_now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64
                },
            ));
        }
        // Lazy-init above guarantees `guard` is `Some` here; fall back
        // to a silent no-op if a future refactor breaks that invariant
        // rather than panicking inside the verdict-gate hot path.
        let envelope = match guard.as_mut() {
            Some(builder) => builder.build(),
            None => return,
        };
        drop(guard);
        let event = AgentEvent::DriverEvent {
            envelope,
            driver_kind: kind,
            summary: summary.to_string(),
            payload,
        };
        let sink_result =
            LogSink::open_phase_at(&logs_root, &self.label, "review", None, self.phase_log_when);
        match sink_result {
            Ok(mut sink) => {
                if let Err(e) = sink.emit(&event) {
                    warn!(error = %e, "review controller: emit driver event failed");
                }
                // Finish is idempotent — the agent-event sink (opened
                // separately in run_review) reaches the same file and
                // will run finish itself with the bead outcome.
                let _ = sink.finish(BeadOutcome::Done);
            }
            Err(e) => {
                warn!(error = %e, "review controller: open phase sink for driver event failed");
            }
        }
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
    use crate::review::runner::ReviewController;
    use loom_driver::identifier::MoleculeId;
    use loom_driver::state::ActiveMolecule;
    use std::ffi::OsStr;
    use std::future::Ready;

    type SpawnFuture = Ready<Result<(SessionOutcome, Option<ExitSignal>), ProtocolError>>;
    type NoopSpawn = fn(SpawnConfig) -> SpawnFuture;
    type NoopController = ProductionReviewController<NoopSpawn, SpawnFuture>;

    fn noop_spawn(_cfg: SpawnConfig) -> SpawnFuture {
        std::future::ready(Ok((
            SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            Some(ExitSignal::Complete),
        )))
    }

    /// FR12 — `loom review`'s phase-end MUST route the reviewer's marker
    /// through the canonical [`decide`] gate function rather than its own
    /// ad-hoc `match` on `exit_code`. This test pins the marker → outcome
    /// mapping that `decide()` produces for the review phase: `COMPLETE`
    /// reaches `Complete`, `BLOCKED`/`CLARIFY` self-reports surface as
    /// `Incomplete` carrying the marker text, and a missing marker routes
    /// to `swallowed-marker` recovery (mapped to `Incomplete`). Combined
    /// with the source-level `decide()` import in `classify_review_phase`,
    /// the two together fence the FR12 contract.
    #[test]
    fn classify_review_phase_routes_marker_through_phase_verdict_decide() {
        // `COMPLETE` + clean exit → review phase passes.
        assert_eq!(
            classify_review_phase(Some(&ExitSignal::Complete), 0),
            ReviewOutcome::Complete,
        );
        // `BLOCKED` self-report surfaces as `Incomplete` carrying the marker.
        match classify_review_phase(
            Some(&ExitSignal::Blocked {
                reason: "missing schema".into(),
            }),
            0,
        ) {
            ReviewOutcome::Incomplete { detail } => assert!(
                detail.contains("LOOM_BLOCKED") && detail.contains("missing schema"),
                "blocked detail missing reason: {detail}",
            ),
            other => panic!("expected Incomplete, got {other:?}"),
        }
        // `CLARIFY` self-report surfaces as `Incomplete` carrying the question.
        match classify_review_phase(
            Some(&ExitSignal::Clarify {
                question: "additive only?".into(),
            }),
            0,
        ) {
            ReviewOutcome::Incomplete { detail } => assert!(
                detail.contains("LOOM_CLARIFY") && detail.contains("additive only?"),
                "clarify detail missing question: {detail}",
            ),
            other => panic!("expected Incomplete, got {other:?}"),
        }
        // None marker → `Recovery::SwallowedMarker` → `Incomplete` carrying
        // the swallowed-marker phrasing.
        match classify_review_phase(None, 0) {
            ReviewOutcome::Incomplete { detail } => assert!(
                detail.contains("swallowed marker"),
                "swallowed-marker text missing: {detail}",
            ),
            other => panic!("expected Incomplete, got {other:?}"),
        }
        // None marker + non-zero exit → exit code surfaces in detail.
        match classify_review_phase(None, 7) {
            ReviewOutcome::Incomplete { detail } => assert!(
                detail.contains('7'),
                "exit code missing from detail: {detail}",
            ),
            other => panic!("expected Incomplete, got {other:?}"),
        }
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

    fn controller(workspace: PathBuf) -> NoopController {
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        ProductionReviewController::new(
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
    ) -> NoopController {
        let manifest = stub_manifest(&workspace);
        ProductionReviewController::new(
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
            matches!(err, ReviewError::NoActiveMolecule(ref s) if s == "loom-harness"),
            "expected NoActiveMolecule(loom-harness), got {err:?}",
        );
    }

    #[tokio::test]
    async fn reset_iteration_is_no_op_when_no_active_molecule() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctrl = controller(dir.path().to_path_buf());
        ctrl.reset_iteration_count().await.unwrap();
    }

    /// Seed a stub spec file at `specs/<label>.md` with an empty
    /// `## Success Criteria` section so `load_review_sources` succeeds in
    /// tests that don't exercise verify/judge bodies.
    fn seed_empty_spec(workspace: &std::path::Path, label: &str) {
        let path = workspace.join(format!("specs/{label}.md"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "## Success Criteria\n\n").unwrap();
    }

    #[tokio::test]
    async fn run_review_translates_zero_exit_into_complete() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        seed_empty_spec(&workspace, "loom-harness");
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let mut ctrl = ProductionReviewController::new(
            BdClient::new(),
            SpecLabel::new("loom-harness"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            |_cfg: SpawnConfig| async move {
                Ok((
                    SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    },
                    Some(ExitSignal::Complete),
                ))
            },
        );
        let outcome = ctrl.run_review().await;
        if let Err(ReviewError::Bd(_)) = outcome {
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
        seed_empty_spec(&workspace, "loom-harness");
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let mut ctrl = ProductionReviewController::new(
            BdClient::new(),
            SpecLabel::new("loom-harness"),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            |_cfg: SpawnConfig| async move {
                // No marker + non-zero exit: the gate routes via
                // SwallowedMarker, and the review classifier folds the exit
                // code into the detail body for human triage.
                Ok((
                    SessionOutcome {
                        exit_code: 7,
                        cost_usd: None,
                    },
                    None,
                ))
            },
        );
        let outcome = ctrl.run_review().await;
        if let Err(ReviewError::Bd(_)) = outcome {
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

    /// The review prompt must instruct the reviewer to walk
    /// `docs/style-rules.md` rule by rule and cite rule id + file/line for
    /// each violation. This is the load-bearing surface for style-rule
    /// conformance — `loom gate verify`'s deterministic audits cannot enforce
    /// the prose rules, so the LLM-judged rubric is the only line of defence.
    #[tokio::test]
    async fn build_review_prompt_includes_style_rule_conformance_walkthrough() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let label = "alpha";
        seed_empty_spec(workspace, label);
        let state = empty_state(workspace);
        let manifest = stub_manifest(workspace);
        let ctrl = ProductionReviewController::new(
            BdClient::new(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace.to_path_buf(),
            state,
            manifest,
            ProfileName::new("base"),
            noop_spawn,
        );
        let prompt = match ctrl.build_review_prompt().await {
            Ok(p) => p,
            Err(ReviewError::Bd(_)) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        };
        assert!(
            prompt.contains("## Style-Rule Conformance"),
            "rubric heading missing: {prompt}",
        );
        assert!(
            prompt.contains("docs/style-rules.md"),
            "style_rules path not pinned in review prompt: {prompt}",
        );
        assert!(
            prompt.contains("Discover the families")
                && prompt.contains("do not assume a fixed prefix list"),
            "family-discovery instruction missing: {prompt}",
        );
        for forbidden in ["**SH-**", "**NX-**", "**RS-**", "**COM-**", "**CLI-**"] {
            assert!(
                !prompt.contains(forbidden),
                "rule-family marker {forbidden} leaked into review prompt: {prompt}",
            );
        }
        assert!(
            prompt.contains("LOOM_REVIEW_FLAG: style-rule"),
            "flag marker not documented in review prompt: {prompt}",
        );
        assert!(
            prompt.contains("rule id"),
            "citation contract (rule id) not described: {prompt}",
        );
        assert!(
            prompt.contains("file and line range") || prompt.contains("file/line range"),
            "citation contract (file/line range) not described: {prompt}",
        );
    }

    #[tokio::test]
    async fn build_review_prompt_inlines_verify_and_judge_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let label = "alpha";
        std::fs::create_dir_all(workspace.join("specs")).unwrap();
        std::fs::create_dir_all(workspace.join("tests/judges")).unwrap();
        std::fs::write(
            workspace.join(format!("specs/{label}.md")),
            "## Success Criteria\n\n\
             - [ ] one\n  [verify](tests/alpha.sh#test_one)\n\
             - [ ] two\n  [judge](tests/judges/alpha.sh#judge_two)\n",
        )
        .unwrap();
        std::fs::write(workspace.join("tests/alpha.sh"), "VERIFY_BODY_MARKER\n").unwrap();
        std::fs::write(
            workspace.join("tests/judges/alpha.sh"),
            "JUDGE_BODY_MARKER\n",
        )
        .unwrap();
        let state = empty_state(workspace);
        let manifest = stub_manifest(workspace);
        let ctrl = ProductionReviewController::new(
            BdClient::new(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace.to_path_buf(),
            state,
            manifest,
            ProfileName::new("base"),
            noop_spawn,
        );
        let prompt = match ctrl.build_review_prompt().await {
            Ok(p) => p,
            Err(ReviewError::Bd(_)) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        };
        assert!(prompt.contains("VERIFY_BODY_MARKER"), "{prompt}");
        assert!(prompt.contains("JUDGE_BODY_MARKER"), "{prompt}");
        assert!(prompt.contains("tests/alpha.sh"), "{prompt}");
        assert!(prompt.contains("tests/judges/alpha.sh"), "{prompt}");
    }

    /// `loom review` must dispatch with the rendered `ReviewContext`
    /// template — `# Post-Epic Review` heading, spec_path, and
    /// scratchpad path all reach the agent prompt — and the same body
    /// must land in `<scratch_dir>/prompt.txt` so post-compaction
    /// `repin.sh` can re-emit the actual phase prompt. Mirror of the
    /// run-side test in `run/production.rs`.
    #[tokio::test]
    async fn run_review_dispatches_rendered_review_template_and_writes_prompt_txt() {
        use std::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let label = "loom-harness";
        seed_empty_spec(&workspace, label);
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let captured: Arc<Mutex<Option<SpawnConfig>>> = Arc::new(Mutex::new(None));
        let captured_for_closure = Arc::clone(&captured);
        let prompt_seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let prompt_seen_inner = Arc::clone(&prompt_seen);
        let mut ctrl = ProductionReviewController::new(
            BdClient::new(),
            SpecLabel::new(label),
            PathBuf::from("/usr/bin/loom"),
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            move |cfg: SpawnConfig| {
                let captured = Arc::clone(&captured_for_closure);
                let prompt_seen = Arc::clone(&prompt_seen_inner);
                async move {
                    let txt = std::fs::read_to_string(cfg.scratch_dir.join("prompt.txt"))
                        .expect("prompt.txt readable");
                    *prompt_seen.lock().unwrap() = Some(txt);
                    *captured.lock().unwrap() = Some(cfg);
                    Ok((
                        SessionOutcome {
                            exit_code: 0,
                            cost_usd: None,
                        },
                        Some(ExitSignal::Complete),
                    ))
                }
            },
        );
        let outcome = ctrl.run_review().await;
        if let Err(ReviewError::Bd(_)) = outcome {
            return;
        }
        outcome.expect("run_review ok");
        let cfg = captured.lock().unwrap().take().expect("closure called");
        assert!(
            cfg.initial_prompt.contains("# Post-Epic Review"),
            "prompt missing template heading: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.initial_prompt.contains("specs/loom-harness.md"),
            "prompt missing spec path: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.initial_prompt.contains(".wrapix/loom/scratch"),
            "prompt missing scratchpad partial: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.repin.orientation.is_empty()
                && cfg.repin.pinned_context.is_empty()
                && cfg.repin.partial_bodies.is_empty(),
            "RePinContent must be empty placeholder; rendered template lives in prompt.txt: {:?}",
            cfg.repin,
        );
        let written = prompt_seen.lock().unwrap().take().expect("prompt.txt seen");
        assert_eq!(written, cfg.initial_prompt);
    }

    /// Regression: `exec_run` (the review → run handoff for auto-iterate)
    /// must release the spec lock before spawning, so the `loom run` child
    /// can acquire it. Mirror of the run-side test in `run/production.rs`.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_run_releases_lock_before_spawning_child() {
        use loom_driver::clock::SystemClock;
        use loom_driver::lock::LockManager;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let state_home = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let state = empty_state(&workspace);
        let manifest = stub_manifest(&workspace);
        let mgr = LockManager::with_state_home(&workspace, state_home.path()).unwrap();
        let label = SpecLabel::new("alpha");
        let clock = SystemClock::new();
        let guard = mgr.acquire_spec_async(&label, &clock).await.unwrap();

        // Stand-in for the `loom` binary; /bin/true is absent on NixOS.
        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut ctrl = ProductionReviewController::new(
            BdClient::new(),
            label.clone(),
            stub,
            workspace,
            state,
            manifest,
            ProfileName::new("base"),
            |_cfg: SpawnConfig| async move {
                Ok((
                    SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    },
                    Some(ExitSignal::Complete),
                ))
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
