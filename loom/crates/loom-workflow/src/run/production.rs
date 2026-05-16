//! Production [`AgentLoopController`] used by the `loom run` binary.
//!
//! Wires `BdClient` for bead lookup/close/clarify, a `tokio::process::Command`
//! shell-out for `exec_review`, and a caller-provided dispatch closure for the
//! actual agent invocation. The closure pattern keeps backend selection
//! (`PiBackend` vs `ClaudeBackend`) inside the binary's `dispatch` match —
//! `loom-workflow` never sees the concrete backend types, mirroring the shape
//! used by `ProductionTodoController` and `run_parallel_batch`.
//!
//! Per-bead profile dispatch is wired through [`build_spawn_config_from_manifest`]:
//! the manifest, CLI `--profile` override, and per-phase fallback all flow
//! into the controller at construction time so `run_bead` resolves the
//! per-bead `image_ref` + `image_source` against the parsed manifest before
//! the agent invocation. A missing manifest entry surfaces as
//! [`RunError::Profile`] — no silent fallback.

use std::path::PathBuf;
use std::sync::Arc;

use loom_driver::agent::{ProtocolError, SpawnConfig};
use loom_driver::bd::{BdClient, Bead, ListOpts, ReadyOpts, UpdateOpts};
use loom_driver::config::Phase;
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::lock::LockGuard;
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::scratch::resolve_scratch_key;
use tokio::process::Command;
use tracing::info;

use super::context::{RunContextInputs, render_run_prompt};
use super::error::RunError;
use super::outcome::{AgentOutcome, SessionResult};
use super::runner::AgentLoopController;
use super::spawn::build_spawn_config_from_manifest;
use crate::review::{GateInputs, PhaseVerdict, RecoveryCause, decide};
use crate::todo::ExitSignal;

/// Wires the [`AgentLoopController`] trait against the real `BdClient`, a
/// caller-provided agent dispatch closure, and a child `loom review` exec for
/// handoff.
///
/// `manifest` / `cli_profile` / `phase_default` are the inputs the per-bead
/// profile resolver chain needs (see
/// [`super::resolve_profile_image`]). They are stored on the controller so
/// every `run_bead` call resolves the bead's `image_ref` + `image_source`
/// from the same parsed manifest, never re-reading it from disk.
///
/// `spawn` is the per-phase dispatch closure: the binary builds it from
/// `dispatch(kind, &spawn_config)` so the workflow stays backend-agnostic.
/// `run_bead` calls it on every retry attempt, so the closure must be `Fn`
/// (callable repeatedly). It receives `(SpawnConfig, BeadId)` — the bead id
/// is passed alongside the spawn config so the closure can open the per-bead
/// JSONL [`LogSink`](loom_driver::logging::LogSink) before dispatch.
pub struct ProductionAgentLoopController<S, F>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = (SessionResult, Option<ExitSignal>)> + Send,
{
    bd: BdClient,
    label: SpecLabel,
    loom_bin: PathBuf,
    workspace: PathBuf,
    manifest: Arc<ProfileImageManifest>,
    cli_profile: Option<ProfileName>,
    phase_default: ProfileName,
    spawn: S,
    /// Spec lock dropped before exec'ing `loom review` so the child can take it.
    lock: Option<LockGuard>,
    /// Workspace-relative path to the style-rules document pinned in the
    /// run prompt. Sourced from `LoomConfig.style_rules` at construction
    /// time via [`Self::with_style_rules`]; defaults to the built-in path
    /// so test fakes that skip the builder still render a valid prompt.
    style_rules: String,
}

impl<S, F> ProductionAgentLoopController<S, F>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = (SessionResult, Option<ExitSignal>)> + Send,
{
    #[expect(clippy::too_many_arguments, reason = "controller construction surface")]
    pub fn new(
        bd: BdClient,
        label: SpecLabel,
        loom_bin: PathBuf,
        workspace: PathBuf,
        manifest: Arc<ProfileImageManifest>,
        cli_profile: Option<ProfileName>,
        phase_default: ProfileName,
        spawn: S,
    ) -> Self {
        Self {
            bd,
            label,
            loom_bin,
            workspace,
            manifest,
            cli_profile,
            phase_default,
            spawn,
            lock: None,
            style_rules: "docs/style-rules.md".to_string(),
        }
    }

    /// Hand the spec lock to the controller so `exec_review` can drop it
    /// before spawning the `loom review` child (which acquires the same lock).
    pub fn with_handoff_lock(mut self, guard: LockGuard) -> Self {
        self.lock = Some(guard);
        self
    }

    /// Override the style-rules pin used in the rendered run prompt.
    /// Production callers thread this from `LoomConfig.style_rules`; tests
    /// rely on the built-in default.
    pub fn with_style_rules(mut self, path: String) -> Self {
        self.style_rules = path;
        self
    }

    fn spec_label_filter(&self) -> String {
        format!("spec:{}", self.label.as_str())
    }
}

impl<S, F> AgentLoopController for ProductionAgentLoopController<S, F>
where
    S: Fn(SpawnConfig, BeadId) -> F + Send,
    F: std::future::Future<Output = (SessionResult, Option<ExitSignal>)> + Send,
{
    async fn next_ready_bead(&mut self) -> Result<Option<Bead>, RunError> {
        let beads = self
            .bd
            .ready(ReadyOpts {
                limit: Some(1),
                label: Some(self.spec_label_filter()),
                exclude_label: vec!["loom:clarify".into(), "loom:blocked".into()],
            })
            .await?;
        Ok(beads.into_iter().next())
    }

    async fn run_bead(
        &mut self,
        bead: &Bead,
        previous_failure: Option<String>,
    ) -> Result<AgentOutcome, RunError> {
        let banner = format!("loom run @ {}", bead.id);
        let is_retry = previous_failure.is_some();
        let key = resolve_scratch_key(Phase::Run, &self.label, Some(&bead.id));
        let scratchpad_path =
            loom_driver::scratch::ScratchSession::scratchpad_path_for(&self.workspace, &key)
                .to_string_lossy()
                .into_owned();
        let initial_prompt = render_run_prompt(RunContextInputs {
            label: self.label.clone(),
            spec_path: format!("specs/{}.md", self.label.as_str()),
            pinned_context: String::new(),
            companion_paths: vec![],
            molecule_id: None,
            issue_id: bead.id.clone(),
            title: bead.title.clone(),
            description: bead.description.clone(),
            previous_failure,
            scratchpad_path,
            style_rules: self.style_rules.clone(),
        })
        .map_err(|e| RunError::Protocol(ProtocolError::Io(std::io::Error::other(e))))?;
        let scratch = loom_driver::scratch::ScratchSession::open(
            &self.workspace,
            &key,
            &initial_prompt,
            &banner,
        )
        .map_err(|source| RunError::Protocol(ProtocolError::Io(source)))?;
        let spawn_config = build_spawn_config_from_manifest(
            &self.manifest,
            bead,
            self.cli_profile.as_ref(),
            &self.phase_default,
            self.workspace.clone(),
            initial_prompt,
            scratch.path().to_path_buf(),
            vec![],
            vec![],
        )?;
        info!(
            bead = %bead.id,
            image_ref = %spawn_config.image_ref,
            retry = is_retry,
            "loom run: dispatching agent",
        );
        let (session, marker) = (self.spawn)(spawn_config, bead.id.clone()).await;
        // Drop happens here at end of scope — scratch dir cleaned up on
        // every exit path (success, failure, panic).
        drop(scratch);
        Ok(classify_session(session, marker))
    }

    async fn apply_clarify(&mut self, bead: &BeadId, question: &str) -> Result<(), RunError> {
        let notes = if question.is_empty() {
            None
        } else {
            Some(question.to_string())
        };
        self.bd
            .update(
                bead,
                UpdateOpts {
                    add_labels: vec!["loom:clarify".to_string()],
                    notes,
                    ..UpdateOpts::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn apply_blocked(
        &mut self,
        bead: &BeadId,
        cause: &str,
        error: &str,
    ) -> Result<(), RunError> {
        // Notes layout pins the cause string at the head so `bd show
        // --notes` greps cleanly for `infra-preflight` / `infra-repeated`
        // even when the raw error body is multi-line. Spec
        // (`loom-harness.md` §"Verdict Gate · Infra failures") names the
        // cause as the routing identifier; the error detail is for human
        // triage only.
        let notes = if error.is_empty() {
            cause.to_string()
        } else {
            format!("{cause}: {error}")
        };
        self.bd
            .update(
                bead,
                UpdateOpts {
                    add_labels: vec!["loom:blocked".to_string()],
                    notes: Some(notes),
                    ..UpdateOpts::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn exec_review(&mut self) -> Result<(), RunError> {
        // Release the spec lock before spawning the child — `loom check` and
        // `loom review` acquire the same lock and would otherwise time out
        // behind us.
        self.lock.take();
        // Molecule-completion handoff (FR1): unconditional `--tree` scope on
        // both, check first then review. Non-zero exit codes are NOT fatal
        // to `run_loop` — they signal concerns that the outer loop drives
        // toward via fix-up beads on the next pass. Spawn failures (the
        // child never started) DO surface as `RunError::Io`.
        let check_status = Command::new(&self.loom_bin)
            .current_dir(&self.workspace)
            .arg("check")
            .arg("--tree")
            .arg("-s")
            .arg(self.label.as_str())
            .status()
            .await?;
        info!(
            spec = %self.label.as_str(),
            exit_code = check_status.code().unwrap_or(-1),
            "loom run: molecule handoff — loom check --tree finished",
        );
        let review_status = Command::new(&self.loom_bin)
            .current_dir(&self.workspace)
            .arg("review")
            .arg("--tree")
            .arg("-s")
            .arg(self.label.as_str())
            .status()
            .await?;
        info!(
            spec = %self.label.as_str(),
            exit_code = review_status.code().unwrap_or(-1),
            "loom run: molecule handoff — loom review --tree finished",
        );
        Ok(())
    }
}

/// Translate a `(SessionResult, Option<ExitSignal>)` pair into an
/// [`AgentOutcome`]. Marker → outcome routing goes through the canonical
/// [`crate::review::decide`] gate function (FR12 — single source of truth);
/// `bd_closed` / `diff_empty` / verify / review observables are not queried
/// at the per-bead exit (they belong to `loom check`'s deterministic pass),
/// so neutral inputs are passed and the gate's output reduces to marker-only
/// routing. A defensive guard for `LOOM_COMPLETE`/`LOOM_NOOP` paired with a
/// non-zero exit code predates the gate call because the spec's decision
/// table does not consider exit code: a marker that disagrees with the
/// kernel's view is surfaced as a failure rather than trusted blindly.
pub fn classify_session(session: SessionResult, marker: Option<ExitSignal>) -> AgentOutcome {
    match session {
        SessionResult::PreflightFailed { error } => AgentOutcome::InfraPreflight { error },
        SessionResult::MidSessionFailed { error } => AgentOutcome::InfraMidSession { error },
        SessionResult::Complete(outcome) => {
            if matches!(marker, Some(ExitSignal::Complete | ExitSignal::Noop))
                && outcome.exit_code != 0
            {
                return AgentOutcome::Failure {
                    error: format!(
                        "agent emitted COMPLETE/NOOP but exited code {}",
                        outcome.exit_code,
                    ),
                };
            }
            verdict_to_outcome(
                decide(marker.as_ref(), neutral_gate_inputs()),
                outcome.exit_code,
            )
        }
    }
}

/// Inputs threaded into [`decide`] when classifying the per-bead exit. The
/// run-phase classifier only knows the marker; bd-closed, diff, verify, and
/// review live in `loom check`'s downstream pass. Passing neutral defaults
/// reduces the gate to marker-only routing — the spec table rows for
/// `COMPLETE`/`NOOP` collapse to `Done` and `None` to `SwallowedMarker`,
/// which is what the in-session classifier needs.
fn neutral_gate_inputs() -> GateInputs {
    GateInputs {
        bd_closed: true,
        diff_empty: false,
        verify_failures: vec![],
        review_flag: None,
    }
}

fn verdict_to_outcome(verdict: PhaseVerdict, exit_code: i32) -> AgentOutcome {
    match verdict {
        PhaseVerdict::Done => AgentOutcome::Success,
        PhaseVerdict::Blocked { reason } => AgentOutcome::Blocked { reason },
        PhaseVerdict::Clarify { question } => AgentOutcome::Clarify { question },
        PhaseVerdict::Recovery {
            cause: RecoveryCause::SwallowedMarker,
        } => AgentOutcome::Failure {
            error: if exit_code == 0 {
                "agent exited 0 without LOOM_COMPLETE / LOOM_NOOP / LOOM_BLOCKED / \
                 LOOM_CLARIFY marker (swallowed marker)"
                    .to_string()
            } else {
                format!("agent exited with code {exit_code}")
            },
        },
        PhaseVerdict::Recovery { cause } => AgentOutcome::Failure {
            error: format!("unexpected gate verdict: {}", cause.as_str()),
        },
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
            ..ListOpts::default()
        })
        .await?;
    Ok(beads)
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
    use loom_driver::agent::SessionOutcome;
    use loom_driver::bd::Label;
    use std::sync::Mutex;

    /// FR12 — `loom run`'s per-bead exit MUST route the agent's marker
    /// through the canonical [`crate::review::decide`] gate function rather
    /// than its own ad-hoc `match`. This test pins the marker → outcome
    /// mapping that `decide()` produces under neutral run-phase inputs:
    /// `BLOCKED`/`CLARIFY` short-circuit, `COMPLETE`/`NOOP` reach Done, and
    /// a missing marker routes to `swallowed-marker` recovery (mapped to
    /// `Failure`). A regression that resurrects an inline classifier here
    /// would only fail this test if it diverged from `decide()`'s output —
    /// but combined with the source-level `decide()` import in
    /// `classify_session`, the two together fence the FR12 contract.
    #[test]
    fn classify_session_routes_marker_through_phase_verdict_decide() {
        let session_ok = || {
            SessionResult::Complete(SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            })
        };
        // `BLOCKED` self-report → terminal `Blocked` (gate row 1).
        match classify_session(
            session_ok(),
            Some(ExitSignal::Blocked {
                reason: "missing schema".into(),
            }),
        ) {
            AgentOutcome::Blocked { reason } => assert_eq!(reason, "missing schema"),
            other => panic!("expected Blocked, got {other:?}"),
        }
        // `CLARIFY` self-report → terminal `Clarify` (gate row 2).
        match classify_session(
            session_ok(),
            Some(ExitSignal::Clarify {
                question: "additive only?".into(),
            }),
        ) {
            AgentOutcome::Clarify { question } => assert_eq!(question, "additive only?"),
            other => panic!("expected Clarify, got {other:?}"),
        }
        // `COMPLETE` + clean exit → `Success` (gate row "Done" with neutral inputs).
        assert_eq!(
            classify_session(session_ok(), Some(ExitSignal::Complete)),
            AgentOutcome::Success,
        );
        // `NOOP` + clean exit → `Success` (gate row "Done" with neutral inputs).
        assert_eq!(
            classify_session(session_ok(), Some(ExitSignal::Noop)),
            AgentOutcome::Success,
        );
        // None marker → `Recovery::SwallowedMarker` → `Failure` carrying
        // the spec's swallowed-marker phrasing.
        match classify_session(session_ok(), None) {
            AgentOutcome::Failure { error } => assert!(
                error.contains("swallowed marker"),
                "swallowed-marker text missing: {error}",
            ),
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    fn write_manifest(dir: &std::path::Path) -> Arc<ProfileImageManifest> {
        let body = r#"{
          "base": { "ref": "localhost/wrapix-base:abc", "source": "/nix/store/aaa-image-base" }
        }"#;
        let path = dir.join("profile-images.json");
        std::fs::write(&path, body).expect("write manifest");
        Arc::new(ProfileImageManifest::from_path(&path).expect("parse manifest"))
    }

    fn bead(id: &str) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: format!("title-{id}"),
            description: "desc".into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![Label::new("profile:base")],
            parent: None,
        }
    }

    #[tokio::test]
    async fn run_bead_invokes_dispatch_closure_with_resolved_spawn_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("ws dir");
        let manifest = write_manifest(dir.path());
        let captured: Arc<Mutex<Option<SpawnConfig>>> = Arc::new(Mutex::new(None));
        let captured_for_closure = Arc::clone(&captured);
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace,
            manifest,
            None,
            ProfileName::new("base"),
            move |cfg: SpawnConfig, _bead_id: BeadId| {
                let captured = Arc::clone(&captured_for_closure);
                async move {
                    *captured.lock().unwrap() = Some(cfg);
                    (
                        SessionResult::Complete(SessionOutcome {
                            exit_code: 0,
                            cost_usd: None,
                        }),
                        Some(ExitSignal::Complete),
                    )
                }
            },
        );
        let outcome = controller
            .run_bead(&bead("wx-1"), None)
            .await
            .expect("run_bead ok");
        assert_eq!(outcome, AgentOutcome::Success);
        let cfg = captured.lock().unwrap().take().expect("closure called");
        assert_eq!(cfg.image_ref, "localhost/wrapix-base:abc");
        assert!(cfg.initial_prompt.contains("wx-1"));
    }

    /// wx-hcolw.4 gate: `loom run` must dispatch with the rendered
    /// [`RunContext`] template — bead title/description, scratchpad path,
    /// and spec_path all reach the agent prompt — and the same body must
    /// land in `<scratch_dir>/prompt.txt` so post-compaction `repin.sh`
    /// can re-emit the actual phase prompt.
    #[tokio::test]
    async fn run_bead_dispatches_rendered_run_template_and_writes_prompt_txt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("ws dir");
        let captured: Arc<Mutex<Option<SpawnConfig>>> = Arc::new(Mutex::new(None));
        let captured_for_closure = Arc::clone(&captured);
        let prompt_seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let prompt_seen_inner = Arc::clone(&prompt_seen);
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("loom-harness"),
            PathBuf::from("/loom/bin"),
            workspace.clone(),
            manifest,
            None,
            ProfileName::new("base"),
            move |cfg: SpawnConfig, _bead_id: BeadId| {
                let captured = Arc::clone(&captured_for_closure);
                let prompt_seen = Arc::clone(&prompt_seen_inner);
                async move {
                    // Read prompt.txt mid-session, while the ScratchSession
                    // guard is still alive — Drop removes the dir on return.
                    let txt = std::fs::read_to_string(cfg.scratch_dir.join("prompt.txt"))
                        .expect("prompt.txt readable");
                    *prompt_seen.lock().unwrap() = Some(txt);
                    *captured.lock().unwrap() = Some(cfg);
                    (
                        SessionResult::Complete(SessionOutcome {
                            exit_code: 0,
                            cost_usd: None,
                        }),
                        Some(ExitSignal::Complete),
                    )
                }
            },
        );
        let bead = Bead {
            id: BeadId::new("wx-99").expect("bead id"),
            title: "Implement the harness".into(),
            description: "wire the per-bead loop".into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![Label::new("profile:base")],
            parent: None,
        };
        controller.run_bead(&bead, None).await.expect("run_bead ok");
        let cfg = captured.lock().unwrap().take().expect("closure called");
        // Rendered template body, not the legacy "loom run: bead <id>" stub.
        assert!(
            cfg.initial_prompt.contains("# Implementation Step"),
            "prompt missing template heading: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.initial_prompt.contains("Implement the harness"),
            "prompt missing bead title: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.initial_prompt.contains("wire the per-bead loop"),
            "prompt missing bead description: {}",
            cfg.initial_prompt,
        );
        assert!(
            cfg.initial_prompt.contains("specs/loom-harness.md"),
            "prompt missing spec path: {}",
            cfg.initial_prompt,
        );
        // prompt.txt must hold the same rendered body so repin.sh
        // surfaces the phase prompt under compaction recovery.
        let written = prompt_seen.lock().unwrap().take().expect("prompt.txt seen");
        assert_eq!(written, cfg.initial_prompt);
    }

    #[tokio::test]
    async fn run_bead_translates_nonzero_exit_code_into_failure_with_error_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("ws dir");
        let manifest = write_manifest(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                // Nonzero exit + no marker = swallowed marker; we want to
                // verify the exit_code path. Pass None marker so the
                // classifier hits the `(None, code) => Failure` branch.
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 42,
                        cost_usd: None,
                    }),
                    None,
                )
            },
        );
        let outcome = controller
            .run_bead(&bead("wx-2"), None)
            .await
            .expect("run_bead ok");
        match outcome {
            AgentOutcome::Failure { error } => {
                assert!(
                    error.contains("42"),
                    "error body should mention exit code 42: {error}"
                );
            }
            other => panic!("non-zero exit must produce Failure, got {other:?}"),
        }
    }

    /// Spec gate: a [`SessionResult::PreflightFailed`] from the dispatch
    /// closure must surface as [`AgentOutcome::InfraPreflight`] so
    /// `process_one_bead` routes it straight to `loom:blocked` cause
    /// `infra-preflight`. Dual to the run-loop unit test — verifies the
    /// production controller plumbing carries the variant intact.
    #[tokio::test]
    async fn run_bead_translates_preflight_failure_into_infra_preflight() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("ws dir");
        let manifest = write_manifest(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::PreflightFailed {
                        error: "podman load failed: image archive missing".into(),
                    },
                    None,
                )
            },
        );
        let outcome = controller
            .run_bead(&bead("wx-3"), None)
            .await
            .expect("run_bead ok");
        match outcome {
            AgentOutcome::InfraPreflight { error } => {
                assert!(
                    error.contains("podman load"),
                    "preflight error must carry detail: {error}",
                );
            }
            other => panic!("expected InfraPreflight, got {other:?}"),
        }
    }

    /// Spec gate: a [`SessionResult::MidSessionFailed`] from the dispatch
    /// closure must surface as [`AgentOutcome::InfraMidSession`] so the
    /// driver-memory budget can absorb one occurrence per `loom run`.
    #[tokio::test]
    async fn run_bead_translates_midsession_failure_into_infra_midsession() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("ws dir");
        let manifest = write_manifest(dir.path());
        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            SpecLabel::new("spec-x"),
            PathBuf::from("/loom/bin"),
            workspace,
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::MidSessionFailed {
                        error: "agent stdout closed: exit 137 (OOM)".into(),
                    },
                    None,
                )
            },
        );
        let outcome = controller
            .run_bead(&bead("wx-4"), None)
            .await
            .expect("run_bead ok");
        match outcome {
            AgentOutcome::InfraMidSession { error } => {
                assert!(
                    error.contains("OOM"),
                    "mid-session error must carry detail: {error}",
                );
            }
            other => panic!("expected InfraMidSession, got {other:?}"),
        }
    }

    /// Regression: `loom run` used to hold the spec lock for its whole
    /// lifetime, so the `loom review` child it spawned at the molecule-complete
    /// handoff timed out trying to acquire the same lock. `exec_review` must
    /// drop the held [`LockGuard`] before spawning, leaving the kernel-level
    /// `flock(2)` available to the child. Verified end-to-end: after a stub
    /// child exits, the lock is reacquirable on a fresh attempt.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_releases_lock_before_spawning_child() {
        use loom_driver::clock::SystemClock;
        use loom_driver::lock::LockManager;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let mgr = LockManager::new(dir.path()).expect("lock manager");
        let label = SpecLabel::new("alpha");
        let clock = SystemClock::new();
        let guard = mgr
            .acquire_spec_async(&label, &clock)
            .await
            .expect("first acquire");

        // Stand-in for the `loom` binary: ignores all args and exits 0.
        // /bin/true does not exist on NixOS, so we ship a script.
        let stub = dir.path().join("loom-stub.sh");
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            label.clone(),
            stub,
            dir.path().to_path_buf(),
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        )
        .with_handoff_lock(guard);

        controller.exec_review().await.expect("exec_review ok");

        // The child has exited and the controller's guard was dropped before
        // the spawn — the lock must be free. A short timeout keeps the test
        // fast on the regression (held-lock) path: it would error in <100ms
        // rather than wait the default 5s.
        let _reacquired = mgr
            .acquire_spec_with_timeout_async(&label, &clock, Duration::from_millis(100))
            .await
            .expect("lock must be reacquirable after exec_review");
    }

    /// FR1: the molecule-completion handoff invokes `loom check --tree`
    /// THEN `loom review --tree` — both unconditionally `--tree`-scoped,
    /// in that order, and both with the spec label threaded through `-s`.
    /// The stub script records each invocation so the test can assert on
    /// the exact argv sequence the production controller emits.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_invokes_loom_check_tree_then_loom_review_tree() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("alpha");

        // Recording stub: appends every invocation's argv (one per line,
        // tab-separated) to argv.log so the test can replay the call order.
        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let stub_body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexit 0\n",
            log = argv_log.to_string_lossy(),
        );
        std::fs::write(&stub, stub_body).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            label.clone(),
            stub,
            dir.path().to_path_buf(),
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );

        controller.exec_review().await.expect("exec_review ok");

        let recorded = std::fs::read_to_string(&argv_log).expect("argv log readable");
        let lines: Vec<&str> = recorded.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "exec_review must spawn exactly two children (check then review): {recorded:?}",
        );
        assert_eq!(
            lines[0], "check --tree -s alpha",
            "first child must be `loom check --tree -s <label>`",
        );
        assert_eq!(
            lines[1], "review --tree -s alpha",
            "second child must be `loom review --tree -s <label>`",
        );
    }

    /// FR1: non-zero exit from `loom check` MUST NOT abort the handoff —
    /// it signals concerns that the outer loop drives toward via fix-up
    /// beads on the next pass. The production controller still spawns
    /// `loom review --tree` after check fails, and `exec_review` returns
    /// `Ok` so `run_loop` can re-poll `bd ready` rather than tearing
    /// down the whole `loom run`.
    #[tokio::test(flavor = "multi_thread")]
    async fn exec_review_continues_to_review_when_check_exits_nonzero() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = write_manifest(dir.path());
        let label = SpecLabel::new("beta");

        // Stub: `check` exits 1 (concerns), every other invocation exits 0.
        // First argv token selects the branch.
        let argv_log = dir.path().join("argv.log");
        let stub = dir.path().join("loom-stub.sh");
        let stub_body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
             case \"$1\" in\n  check) exit 1 ;;\n  *) exit 0 ;;\nesac\n",
            log = argv_log.to_string_lossy(),
        );
        std::fs::write(&stub, stub_body).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut controller = ProductionAgentLoopController::new(
            BdClient::new(),
            label.clone(),
            stub,
            dir.path().to_path_buf(),
            manifest,
            None,
            ProfileName::new("base"),
            |_cfg: SpawnConfig, _bead_id: BeadId| async move {
                (
                    SessionResult::Complete(SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    }),
                    Some(ExitSignal::Complete),
                )
            },
        );

        controller
            .exec_review()
            .await
            .expect("non-zero check exit must not produce RunError");

        let recorded = std::fs::read_to_string(&argv_log).expect("argv log readable");
        let lines: Vec<&str> = recorded.lines().collect();
        assert_eq!(
            lines,
            vec!["check --tree -s beta", "review --tree -s beta"],
            "review must still run even when check signals concerns",
        );
    }
}
