use loom_driver::bd::{Bead, Label};
use loom_driver::identifier::BeadId;

use super::error::CheckError;
use super::iteration::IterationCap;
use super::verdict::{CheckVerdict, diff_new_bead_ids};

/// Side-effect surface the [`check_loop`] driver depends on.
///
/// The trait abstracts the BdClient + AgentBackend + git wiring so the
/// verdict logic stays pure-ish and is exercised under a fake without
/// spawning a real container or touching the working tree. The binary wires
/// the methods to:
///
/// - `pre_snapshot` / `post_snapshot` → `BdClient::list { label: "spec:<L>" }`
/// - `blocked_ids` / `clarify_ids` → filter the same list for `loom:blocked`
///   and `loom:clarify` respectively
/// - `run_review` → render check.md, build SpawnConfig, drive
///   `AgentBackend`, tee the event stream into the log sink, parse the
///   exit signal
/// - `iteration_count` / `set_iteration_count` / `reset_iteration_count` →
///   the `iteration_count` column in `loom-driver`'s state DB
/// - `apply_clarify` → `BdClient::update --add-label loom:clarify`
/// - `git_push` / `beads_push` → `tokio::process::Command` shell-outs
/// - `exec_run` → `tokio::process::Command::new("loom").arg("run")…`
pub trait CheckController: Send {
    /// Run the reviewer agent. Returns when the agent emits a terminal
    /// signal or fails. The implementation tees the event stream into the
    /// per-bead JSONL log alongside the terminal renderer.
    fn run_review(
        &mut self,
    ) -> impl std::future::Future<Output = Result<ReviewOutcome, CheckError>> + Send;

    /// Return every bead carrying `spec:<label>` at this moment. Order is
    /// stable (creation order) so the driver's `before`/`after` diff is
    /// deterministic.
    fn list_spec_beads(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Vec<Bead>, CheckError>> + Send;

    /// Read the persisted iteration counter for the active spec.
    fn iteration_count(
        &mut self,
    ) -> impl std::future::Future<Output = Result<u32, CheckError>> + Send;

    /// Persist the next iteration counter value.
    fn set_iteration_count(
        &mut self,
        next: u32,
    ) -> impl std::future::Future<Output = Result<(), CheckError>> + Send;

    /// Reset the iteration counter to zero (clean push path).
    fn reset_iteration_count(
        &mut self,
    ) -> impl std::future::Future<Output = Result<(), CheckError>> + Send;

    /// Add the `loom:clarify` label to a fix-up bead with the cap-reached
    /// note in its update.
    fn apply_clarify(
        &mut self,
        bead: &BeadId,
        reason: &str,
    ) -> impl std::future::Future<Output = Result<(), CheckError>> + Send;

    /// `git push` — code-only push. Errors map to
    /// [`CheckError::GitPushFailed`] or [`CheckError::DetachedHead`].
    fn git_push(&mut self) -> impl std::future::Future<Output = Result<(), CheckError>> + Send;

    /// `beads-push` (Dolt branch sync). Errors map to
    /// [`CheckError::BeadsPushFailed`] — `git push` already succeeded by
    /// the time this runs, so the caller treats this as a separate exit.
    fn beads_push(&mut self) -> impl std::future::Future<Output = Result<(), CheckError>> + Send;

    /// `exec loom run -s <label>` for auto-iteration. Implementations
    /// `exec` (replace process) on success; the future resolves only on
    /// failure to launch.
    fn exec_run(&mut self) -> impl std::future::Future<Output = Result<(), CheckError>> + Send;

    /// Emit a driver-side event into the controller's event sink (the
    /// per-spec phase JSONL log + terminal renderer). Driver events
    /// carry `Source::Driver` and a free-form `kind` so the renderer's
    /// fallback path can show them without a per-kind handler. R7
    /// (wx-r9tmc) routes the four spec'd `push_gate_*` kinds through
    /// here. Production callers thread an `EnvelopeBuilder` (R1) for
    /// the live envelope; the default impl is a no-op so test fakes
    /// that don't care about event emission keep working.
    fn emit_driver_event(&mut self, _kind: &str, _summary: &str, _payload: serde_json::Value) {}
}

/// What the reviewer agent produced. The driver only branches on
/// `Complete`; anything else aborts the gate before the post-snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewOutcome {
    /// `LOOM_COMPLETE` observed; the reviewer finished cleanly.
    Complete,

    /// Agent terminated without `LOOM_COMPLETE` (crashed, hit budget,
    /// emitted `LOOM_BLOCKED`/`LOOM_CLARIFY`). String body is surfaced
    /// in the [`CheckError::ReviewIncomplete`] variant.
    Incomplete { detail: String },
}

/// Final state after the gate runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    /// Push succeeded; iteration counter was reset.
    Pushed,

    /// `PushBlocked` verdict — gate stopped without pushing because at
    /// least one molecule bead carries `loom:blocked` or `loom:clarify`.
    /// Caller surfaces both ID lists to the user via the `loom msg`
    /// pointer.
    PushBlocked {
        blocked_ids: Vec<BeadId>,
        clarify_ids: Vec<BeadId>,
    },

    /// Auto-iteration was triggered. The driver execs `loom run`; if the
    /// `exec` future resolves at all (i.e. didn't replace this process)
    /// the caller receives this variant so it can surface the failure or
    /// continue testing under a fake.
    AutoIterated { next_iteration: u32 },

    /// Iteration cap reached; newest fix-up bead got `loom:clarify`.
    Escalated { escalate_id: BeadId, cap: u32 },
}

/// Drive one `loom check` invocation through the gate.
///
/// 1. Snapshot beads carrying `spec:<label>` (`pre`).
/// 2. Run the reviewer agent.
/// 3. Snapshot again (`post`); compute new bead IDs and clarify membership.
/// 4. Apply the verdict (push / clarify-stop / auto-iterate / escalate).
pub async fn check_loop<C: CheckController>(
    controller: &mut C,
    cap: IterationCap,
) -> Result<CheckResult, CheckError> {
    let pre = controller.list_spec_beads().await?;
    let pre_ids: Vec<BeadId> = pre.iter().map(|b| b.id.clone()).collect();

    match controller.run_review().await? {
        ReviewOutcome::Complete => {}
        ReviewOutcome::Incomplete { detail } => {
            return Err(CheckError::ReviewIncomplete(detail));
        }
    }

    let post = controller.list_spec_beads().await?;
    let post_ids: Vec<BeadId> = post.iter().map(|b| b.id.clone()).collect();
    let new_ids = diff_new_bead_ids(&pre_ids, &post_ids);
    let blocked_ids: Vec<BeadId> = post
        .iter()
        .filter(|b| b.labels.iter().any(Label::is_blocked))
        .map(|b| b.id.clone())
        .collect();
    let clarify_ids: Vec<BeadId> = post
        .iter()
        .filter(|b| b.labels.iter().any(Label::is_clarify))
        .map(|b| b.id.clone())
        .collect();

    let verdict = decide_verdict(&new_ids, &blocked_ids, &clarify_ids, cap, controller).await?;
    apply_verdict(controller, verdict).await
}

/// Pure-ish branch picker: resolves the four verdict shapes from the
/// snapshot diff plus the persisted iteration counter.
async fn decide_verdict<C: CheckController>(
    new_ids: &[BeadId],
    blocked_ids: &[BeadId],
    clarify_ids: &[BeadId],
    cap: IterationCap,
    controller: &mut C,
) -> Result<CheckVerdict, CheckError> {
    if !blocked_ids.is_empty() || !clarify_ids.is_empty() {
        return Ok(CheckVerdict::PushBlocked {
            blocked_ids: blocked_ids.to_vec(),
            clarify_ids: clarify_ids.to_vec(),
        });
    }

    let Some(newest) = new_ids.last() else {
        return Ok(CheckVerdict::Clean);
    };

    let current = controller.iteration_count().await?;
    if cap.is_exhausted(current) {
        return Ok(CheckVerdict::IterationCap {
            new_bead_ids: new_ids.to_vec(),
            escalate_id: newest.clone(),
            cap: cap.max,
        });
    }

    Ok(CheckVerdict::AutoIterate {
        new_bead_ids: new_ids.to_vec(),
        next_iteration: current + 1,
    })
}

async fn apply_verdict<C: CheckController>(
    controller: &mut C,
    verdict: CheckVerdict,
) -> Result<CheckResult, CheckError> {
    // R7 (wx-r9tmc): every gate walk emits `push_gate_walk` first so
    // the JSONL replay carries a fence between the reviewer's output
    // and the verdict-application sequence below. The four kind-
    // specific events follow per the spec table in E1.
    controller.emit_driver_event(
        "push_gate_walk",
        "push gate evaluating verdict",
        serde_json::json!({"verdict": verdict_label(&verdict)}),
    );
    match verdict {
        CheckVerdict::Clean => {
            controller.emit_driver_event(
                "push_gate_clean",
                "verdict clean — pushing code + beads, resetting iteration counter",
                serde_json::json!({}),
            );
            controller.reset_iteration_count().await?;
            controller.git_push().await?;
            controller.beads_push().await?;
            Ok(CheckResult::Pushed)
        }
        CheckVerdict::PushBlocked {
            blocked_ids,
            clarify_ids,
        } => {
            controller.emit_driver_event(
                "push_gate_refuse",
                "verdict push-blocked — molecule beads carry loom:blocked or loom:clarify",
                serde_json::json!({
                    "blocked_ids": blocked_ids.iter().map(|b| b.to_string()).collect::<Vec<_>>(),
                    "clarify_ids": clarify_ids.iter().map(|b| b.to_string()).collect::<Vec<_>>(),
                }),
            );
            Ok(CheckResult::PushBlocked {
                blocked_ids,
                clarify_ids,
            })
        }
        CheckVerdict::AutoIterate {
            next_iteration,
            new_bead_ids,
        } => {
            controller.emit_driver_event(
                "push_gate_walk",
                "verdict auto-iterate — fix-up beads detected, re-entering loom run",
                serde_json::json!({
                    "next_iteration": next_iteration,
                    "new_bead_ids": new_bead_ids.iter().map(|b| b.to_string()).collect::<Vec<_>>(),
                }),
            );
            controller.set_iteration_count(next_iteration).await?;
            controller.exec_run().await?;
            Ok(CheckResult::AutoIterated { next_iteration })
        }
        CheckVerdict::IterationCap {
            escalate_id,
            cap: cap_value,
            ..
        } => {
            let reason = format!(
                "Iteration cap ({cap_value}) reached: review kept finding fix-up work. Human input needed before resuming."
            );
            controller.emit_driver_event(
                "push_gate_exhausted",
                "verdict cap-reached — escalating to clarify",
                serde_json::json!({
                    "escalate_id": escalate_id.to_string(),
                    "cap": cap_value,
                }),
            );
            controller.apply_clarify(&escalate_id, &reason).await?;
            Ok(CheckResult::Escalated {
                escalate_id,
                cap: cap_value,
            })
        }
    }
}

/// Compact label describing the verdict shape — used as the `verdict`
/// field on the leading `push_gate_walk` event so a replay can tell at
/// a glance which branch the gate took.
fn verdict_label(verdict: &CheckVerdict) -> &'static str {
    match verdict {
        CheckVerdict::Clean => "clean",
        CheckVerdict::PushBlocked { .. } => "push_blocked",
        CheckVerdict::AutoIterate { .. } => "auto_iterate",
        CheckVerdict::IterationCap { .. } => "iteration_cap",
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;
    use loom_driver::bd::Bead;

    #[derive(Default)]
    struct FakeController {
        review: Option<ReviewOutcome>,
        pre_beads: Vec<Bead>,
        post_beads: Vec<Bead>,
        list_calls: u32,
        iter_count: u32,
        set_iter_calls: Vec<u32>,
        reset_iter_calls: u32,
        apply_clarify_calls: Vec<(BeadId, String)>,
        git_push_calls: u32,
        beads_push_calls: u32,
        exec_run_calls: u32,
        /// R7 — capture the (kind, summary, payload) tuple for every
        /// `emit_driver_event` so tests can pin the verdict-gate
        /// emission sequence.
        driver_events: Vec<(String, String, serde_json::Value)>,
    }

    impl CheckController for FakeController {
        async fn run_review(&mut self) -> Result<ReviewOutcome, CheckError> {
            Ok(self.review.clone().unwrap_or(ReviewOutcome::Complete))
        }

        async fn list_spec_beads(&mut self) -> Result<Vec<Bead>, CheckError> {
            self.list_calls += 1;
            if self.list_calls == 1 {
                Ok(self.pre_beads.clone())
            } else {
                Ok(self.post_beads.clone())
            }
        }

        async fn iteration_count(&mut self) -> Result<u32, CheckError> {
            Ok(self.iter_count)
        }

        async fn set_iteration_count(&mut self, next: u32) -> Result<(), CheckError> {
            self.set_iter_calls.push(next);
            self.iter_count = next;
            Ok(())
        }

        async fn reset_iteration_count(&mut self) -> Result<(), CheckError> {
            self.reset_iter_calls += 1;
            self.iter_count = 0;
            Ok(())
        }

        async fn apply_clarify(&mut self, bead: &BeadId, reason: &str) -> Result<(), CheckError> {
            self.apply_clarify_calls
                .push((bead.clone(), reason.to_string()));
            Ok(())
        }

        async fn git_push(&mut self) -> Result<(), CheckError> {
            self.git_push_calls += 1;
            Ok(())
        }

        async fn beads_push(&mut self) -> Result<(), CheckError> {
            self.beads_push_calls += 1;
            Ok(())
        }

        async fn exec_run(&mut self) -> Result<(), CheckError> {
            self.exec_run_calls += 1;
            Ok(())
        }

        fn emit_driver_event(&mut self, kind: &str, summary: &str, payload: serde_json::Value) {
            self.driver_events
                .push((kind.to_string(), summary.to_string(), payload));
        }
    }

    fn bead(id: &str, labels: &[&str]) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: format!("title for {id}"),
            description: String::new(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: labels.iter().map(|s| Label::new(*s)).collect(),
        }
    }

    #[tokio::test]
    async fn clean_review_pushes_and_resets_counter() -> Result<(), CheckError> {
        let mut c = FakeController {
            iter_count: 2,
            pre_beads: vec![bead("wx-1", &["spec:loom-harness"])],
            post_beads: vec![bead("wx-1", &["spec:loom-harness"])],
            ..FakeController::default()
        };

        let result = check_loop(&mut c, IterationCap::default()).await?;
        assert_eq!(result, CheckResult::Pushed);
        assert_eq!(c.git_push_calls, 1);
        assert_eq!(c.beads_push_calls, 1);
        assert_eq!(c.reset_iter_calls, 1, "counter resets on clean push");
        assert_eq!(c.exec_run_calls, 0, "no auto-iterate on clean push");
        // R7 (wx-r9tmc) — the verdict-gate fence emits `push_gate_walk`
        // first, then `push_gate_clean` for the clean-push branch.
        let kinds: Vec<&str> = c.driver_events.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(kinds, vec!["push_gate_walk", "push_gate_clean"]);
        Ok(())
    }

    /// R7 — the `PushBlocked` verdict emits `push_gate_walk` then
    /// `push_gate_refuse` carrying both ID lists in its payload.
    #[tokio::test]
    async fn push_blocked_emits_refuse_with_id_payload() -> Result<(), CheckError> {
        let mut c = FakeController {
            pre_beads: vec![bead("wx-1", &["spec:loom-harness"])],
            post_beads: vec![
                bead("wx-1", &["spec:loom-harness"]),
                bead("wx-2", &["spec:loom-harness", "loom:blocked"]),
                bead("wx-3", &["spec:loom-harness", "loom:clarify"]),
            ],
            ..FakeController::default()
        };
        let _ = check_loop(&mut c, IterationCap::default()).await?;
        let kinds: Vec<&str> = c.driver_events.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(kinds, vec!["push_gate_walk", "push_gate_refuse"]);
        let refuse = c
            .driver_events
            .iter()
            .find(|(k, _, _)| k == "push_gate_refuse")
            .expect("refuse event present");
        assert!(
            refuse.2["blocked_ids"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == "wx-2")),
        );
        assert!(
            refuse.2["clarify_ids"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == "wx-3")),
        );
        Ok(())
    }

    /// R7 — the `IterationCap` verdict emits `push_gate_walk` then
    /// `push_gate_exhausted` carrying the escalate-id and the cap.
    #[tokio::test]
    async fn iteration_cap_emits_exhausted_with_cap_payload() -> Result<(), CheckError> {
        let mut c = FakeController {
            iter_count: 3,
            pre_beads: vec![bead("wx-1", &["spec:loom-harness"])],
            post_beads: vec![
                bead("wx-1", &["spec:loom-harness"]),
                bead("wx-cap", &["spec:loom-harness"]),
            ],
            ..FakeController::default()
        };
        let _ = check_loop(&mut c, IterationCap { max: 3 }).await?;
        let kinds: Vec<&str> = c.driver_events.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(kinds, vec!["push_gate_walk", "push_gate_exhausted"]);
        let exhausted = c
            .driver_events
            .iter()
            .find(|(k, _, _)| k == "push_gate_exhausted")
            .expect("exhausted event present");
        assert_eq!(exhausted.2["escalate_id"].as_str(), Some("wx-cap"));
        assert_eq!(exhausted.2["cap"].as_u64(), Some(3));
        Ok(())
    }

    #[tokio::test]
    async fn clarify_present_stops_without_pushing() -> Result<(), CheckError> {
        let mut c = FakeController {
            pre_beads: vec![bead("wx-1", &["spec:loom-harness"])],
            post_beads: vec![
                bead("wx-1", &["spec:loom-harness"]),
                bead("wx-2", &["spec:loom-harness", "loom:clarify"]),
            ],
            ..FakeController::default()
        };

        let result = check_loop(&mut c, IterationCap::default()).await?;
        match result {
            CheckResult::PushBlocked {
                blocked_ids,
                clarify_ids,
            } => {
                assert!(blocked_ids.is_empty(), "no blocked beads in this scenario");
                assert_eq!(clarify_ids, vec![BeadId::new("wx-2").expect("valid")]);
            }
            other => panic!("expected PushBlocked, got {other:?}"),
        }
        assert_eq!(c.git_push_calls, 0, "clarify never pushes");
        assert_eq!(c.beads_push_calls, 0, "clarify never beads-pushes");
        assert_eq!(c.exec_run_calls, 0, "clarify never auto-iterates");
        Ok(())
    }

    #[tokio::test]
    async fn pre_existing_clarify_blocks_push_even_when_no_new_beads() -> Result<(), CheckError> {
        let mut c = FakeController {
            pre_beads: vec![bead("wx-1", &["spec:loom-harness", "loom:clarify"])],
            post_beads: vec![bead("wx-1", &["spec:loom-harness", "loom:clarify"])],
            ..FakeController::default()
        };

        let result = check_loop(&mut c, IterationCap::default()).await?;
        assert!(matches!(result, CheckResult::PushBlocked { .. }));
        assert_eq!(c.git_push_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn blocked_present_stops_without_pushing() -> Result<(), CheckError> {
        let mut c = FakeController {
            pre_beads: vec![bead("wx-1", &["spec:loom-harness"])],
            post_beads: vec![
                bead("wx-1", &["spec:loom-harness"]),
                bead("wx-2", &["spec:loom-harness", "loom:blocked"]),
            ],
            ..FakeController::default()
        };

        let result = check_loop(&mut c, IterationCap::default()).await?;
        match result {
            CheckResult::PushBlocked {
                blocked_ids,
                clarify_ids,
            } => {
                assert_eq!(blocked_ids, vec![BeadId::new("wx-2").expect("valid")]);
                assert!(clarify_ids.is_empty(), "no clarify beads in this scenario");
            }
            other => panic!("expected PushBlocked, got {other:?}"),
        }
        assert_eq!(c.git_push_calls, 0, "blocked never pushes");
        assert_eq!(c.beads_push_calls, 0, "blocked never beads-pushes");
        assert_eq!(c.exec_run_calls, 0, "blocked never auto-iterates");
        Ok(())
    }

    #[tokio::test]
    async fn pre_existing_blocked_blocks_push_even_when_no_new_beads() -> Result<(), CheckError> {
        let mut c = FakeController {
            pre_beads: vec![bead("wx-1", &["spec:loom-harness", "loom:blocked"])],
            post_beads: vec![bead("wx-1", &["spec:loom-harness", "loom:blocked"])],
            ..FakeController::default()
        };

        let result = check_loop(&mut c, IterationCap::default()).await?;
        match result {
            CheckResult::PushBlocked {
                blocked_ids,
                clarify_ids,
            } => {
                assert_eq!(blocked_ids, vec![BeadId::new("wx-1").expect("valid")]);
                assert!(clarify_ids.is_empty());
            }
            other => panic!("expected PushBlocked, got {other:?}"),
        }
        assert_eq!(c.git_push_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn blocked_and_clarify_together_surface_both_lists() -> Result<(), CheckError> {
        let mut c = FakeController {
            pre_beads: vec![bead("wx-1", &["spec:loom-harness"])],
            post_beads: vec![
                bead("wx-1", &["spec:loom-harness"]),
                bead("wx-2", &["spec:loom-harness", "loom:blocked"]),
                bead("wx-3", &["spec:loom-harness", "loom:clarify"]),
            ],
            ..FakeController::default()
        };

        let result = check_loop(&mut c, IterationCap::default()).await?;
        match result {
            CheckResult::PushBlocked {
                blocked_ids,
                clarify_ids,
            } => {
                assert_eq!(blocked_ids, vec![BeadId::new("wx-2").expect("valid")]);
                assert_eq!(clarify_ids, vec![BeadId::new("wx-3").expect("valid")]);
            }
            other => panic!("expected PushBlocked, got {other:?}"),
        }
        assert_eq!(c.git_push_calls, 0);
        assert_eq!(c.beads_push_calls, 0);
        assert_eq!(c.exec_run_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn fix_up_beads_under_cap_auto_iterate() -> Result<(), CheckError> {
        let mut c = FakeController {
            iter_count: 0,
            pre_beads: vec![bead("wx-1", &["spec:loom-harness"])],
            post_beads: vec![
                bead("wx-1", &["spec:loom-harness"]),
                bead("wx-2", &["spec:loom-harness"]),
            ],
            ..FakeController::default()
        };

        let result = check_loop(&mut c, IterationCap::new(3)).await?;
        match result {
            CheckResult::AutoIterated { next_iteration } => {
                assert_eq!(next_iteration, 1);
            }
            other => panic!("expected AutoIterated, got {other:?}"),
        }
        assert_eq!(c.set_iter_calls, vec![1], "counter incremented before exec");
        assert_eq!(c.exec_run_calls, 1, "exec loom run on auto-iterate");
        assert_eq!(c.git_push_calls, 0, "auto-iterate never pushes");
        assert!(c.apply_clarify_calls.is_empty(), "no escalation under cap");
        Ok(())
    }

    #[tokio::test]
    async fn iteration_cap_escalates_newest_fix_up_to_clarify() -> Result<(), CheckError> {
        let mut c = FakeController {
            iter_count: 3,
            pre_beads: vec![bead("wx-1", &["spec:loom-harness"])],
            post_beads: vec![
                bead("wx-1", &["spec:loom-harness"]),
                bead("wx-2", &["spec:loom-harness"]),
                bead("wx-3", &["spec:loom-harness"]),
            ],
            ..FakeController::default()
        };

        let result = check_loop(&mut c, IterationCap::new(3)).await?;
        match result {
            CheckResult::Escalated { escalate_id, cap } => {
                assert_eq!(
                    escalate_id,
                    BeadId::new("wx-3").expect("valid"),
                    "newest fix-up"
                );
                assert_eq!(cap, 3);
            }
            other => panic!("expected Escalated, got {other:?}"),
        }
        assert_eq!(c.apply_clarify_calls.len(), 1);
        assert_eq!(
            c.apply_clarify_calls[0].0,
            BeadId::new("wx-3").expect("valid")
        );
        assert!(
            c.apply_clarify_calls[0].1.contains("Iteration cap"),
            "reason names the cap"
        );
        assert_eq!(c.git_push_calls, 0);
        assert_eq!(c.exec_run_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn review_incomplete_aborts_before_post_snapshot() -> Result<(), CheckError> {
        let mut c = FakeController {
            review: Some(ReviewOutcome::Incomplete {
                detail: "no result line".into(),
            }),
            pre_beads: vec![bead("wx-1", &["spec:loom-harness"])],
            post_beads: vec![bead("wx-1", &["spec:loom-harness"])],
            ..FakeController::default()
        };

        let err = check_loop(&mut c, IterationCap::default()).await.err();
        assert!(matches!(err, Some(CheckError::ReviewIncomplete(_))));
        assert_eq!(c.list_calls, 1, "post snapshot not taken on review failure");
        assert_eq!(c.git_push_calls, 0);
        Ok(())
    }
}
