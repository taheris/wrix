use loom_core::bd::{Bead, Label};
use loom_core::identifier::BeadId;

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
/// - `clarify_ids` → filter the same list for `loom:clarify`
/// - `run_review` → render check.md, build SpawnConfig, drive
///   `AgentBackend`, tee the event stream into the log sink, parse the
///   exit signal
/// - `iteration_count` / `set_iteration_count` / `reset_iteration_count` →
///   the `iteration_count` column in `loom-core`'s state DB
/// - `apply_clarify` → `BdClient::update --add-label loom:clarify`
/// - `git_push` / `beads_push` → `tokio::process::Command` shell-outs
/// - `exec_run` → `tokio::process::Command::new("loom").arg("run")…`
pub trait CheckController: Send {
    /// Run the reviewer agent. Returns when the agent emits a terminal
    /// signal or fails. The implementation tees the event stream into the
    /// per-bead NDJSON log alongside the terminal renderer.
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

    /// `Clarify` verdict — gate stopped without pushing. Caller surfaces
    /// the IDs to the user via the `loom msg` pointer.
    Clarified { clarify_ids: Vec<BeadId> },

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
    let clarify_ids: Vec<BeadId> = post
        .iter()
        .filter(|b| b.labels.iter().any(Label::is_clarify))
        .map(|b| b.id.clone())
        .collect();

    let verdict = decide_verdict(&new_ids, &clarify_ids, cap, controller).await?;
    apply_verdict(controller, verdict).await
}

/// Pure-ish branch picker: resolves the four verdict shapes from the
/// snapshot diff plus the persisted iteration counter.
async fn decide_verdict<C: CheckController>(
    new_ids: &[BeadId],
    clarify_ids: &[BeadId],
    cap: IterationCap,
    controller: &mut C,
) -> Result<CheckVerdict, CheckError> {
    if !clarify_ids.is_empty() {
        return Ok(CheckVerdict::Clarify {
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
    match verdict {
        CheckVerdict::Clean => {
            controller.reset_iteration_count().await?;
            controller.git_push().await?;
            controller.beads_push().await?;
            Ok(CheckResult::Pushed)
        }
        CheckVerdict::Clarify { clarify_ids } => Ok(CheckResult::Clarified { clarify_ids }),
        CheckVerdict::AutoIterate { next_iteration, .. } => {
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
            controller.apply_clarify(&escalate_id, &reason).await?;
            Ok(CheckResult::Escalated {
                escalate_id,
                cap: cap_value,
            })
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use loom_core::bd::Bead;

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
            CheckResult::Clarified { clarify_ids } => {
                assert_eq!(clarify_ids, vec![BeadId::new("wx-2").expect("valid")]);
            }
            other => panic!("expected Clarified, got {other:?}"),
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
        assert!(matches!(result, CheckResult::Clarified { .. }));
        assert_eq!(c.git_push_calls, 0);
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
