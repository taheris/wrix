use loom_core::bd::Bead;
use loom_core::identifier::BeadId;

use super::error::RunError;
use super::outcome::{AgentOutcome, BeadResult};
use super::retry::{RetryDecision, RetryPolicy};

/// Loop-termination policy for `loom run`. `Continuous` is the default — the
/// loop pulls beads until the molecule is complete, then hands off to
/// `loom check`. `Once` exits after the first bead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Once,
    Continuous,
}

/// Summary of one [`run_loop`] invocation. Surfaces what happened so callers
/// can return a meaningful exit code and tests can assert on the path taken.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RunSummary {
    /// Beads that ran to a terminal state (closed or clarified).
    pub beads_processed: u32,
    /// Beads that exhausted retries and got the `loom:clarify` label.
    pub beads_clarified: u32,
    /// `bd ready` returned no candidate, signalling the molecule is complete.
    pub molecule_complete: bool,
    /// `loom check` was exec'd (continuous mode + molecule complete).
    pub execed_check: bool,
}

/// Side-effect surface the [`run_loop`] driver depends on.
///
/// The trait abstracts the concrete BdClient + AgentBackend + LogSink wiring
/// so the loop logic stays pure-ish and can be exercised under a fake without
/// spawning a real container. The binary wires this to:
///
/// - `next_ready_bead` → `BdClient::list` filtered by ready label
/// - `run_bead` → render template, build SpawnConfig, drive `AgentBackend`,
///   tee `AgentEvent` stream into `LogSink`, parse exit signal
/// - `close_bead` → `BdClient::close`
/// - `apply_clarify` → `BdClient::update --add-label loom:clarify`
/// - `exec_check` → `tokio::process::Command::new("loom").arg("check")…`
pub trait AgentLoopController: Send {
    /// Pull the next ready bead. Returns `None` when the molecule is done.
    fn next_ready_bead(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Option<Bead>, RunError>> + Send;

    /// Run one agent attempt against `bead`, threading `previous_failure` if
    /// any (the wrapped truncation lives in `loom-templates`).
    fn run_bead(
        &mut self,
        bead: &Bead,
        previous_failure: Option<String>,
    ) -> impl std::future::Future<Output = Result<AgentOutcome, RunError>> + Send;

    /// `bd close <id>` after a successful bead.
    fn close_bead(
        &mut self,
        bead: &BeadId,
    ) -> impl std::future::Future<Output = Result<(), RunError>> + Send;

    /// Add the `loom:clarify` label after retries are exhausted.
    fn apply_clarify(
        &mut self,
        bead: &BeadId,
    ) -> impl std::future::Future<Output = Result<(), RunError>> + Send;

    /// Hand off to `loom check` on molecule completion (continuous mode).
    fn exec_check(&mut self) -> impl std::future::Future<Output = Result<(), RunError>> + Send;
}

/// Run the per-bead loop.
///
/// The function is deliberately not generic over `RetryPolicy` (the policy is
/// a small `Copy` value) but it is generic over [`AgentLoopController`] so the
/// binary and tests can supply different concrete impls. Returns when:
///
/// - `mode == Once` and one bead finished (success or clarify), or
/// - `mode == Continuous` and `next_ready_bead` returned `None` (molecule
///   complete) — `exec_check` is invoked before returning.
pub async fn run_loop<C: AgentLoopController>(
    controller: &mut C,
    mode: RunMode,
    policy: RetryPolicy,
) -> Result<RunSummary, RunError> {
    let mut summary = RunSummary::default();
    loop {
        let bead = match controller.next_ready_bead().await? {
            Some(b) => b,
            None => {
                summary.molecule_complete = true;
                if matches!(mode, RunMode::Continuous) {
                    controller.exec_check().await?;
                    summary.execed_check = true;
                }
                break;
            }
        };

        let result = process_one_bead(controller, &bead, policy).await?;
        summary.beads_processed += 1;

        match result {
            BeadResult::Done => {
                controller.close_bead(&bead.id).await?;
            }
            BeadResult::Clarified { .. } => {
                controller.apply_clarify(&bead.id).await?;
                summary.beads_clarified += 1;
            }
        }

        if matches!(mode, RunMode::Once) {
            break;
        }
    }
    Ok(summary)
}

/// Run a single bead through the retry state machine.
async fn process_one_bead<C: AgentLoopController>(
    controller: &mut C,
    bead: &Bead,
    policy: RetryPolicy,
) -> Result<BeadResult, RunError> {
    let mut retries_used: u32 = 0;
    let mut previous_failure: Option<String> = None;
    loop {
        match controller.run_bead(bead, previous_failure.clone()).await? {
            AgentOutcome::Success => return Ok(BeadResult::Done),
            AgentOutcome::Failure { error } => match policy.decide(retries_used, error) {
                RetryDecision::Retry {
                    previous_failure: pf,
                } => {
                    retries_used += 1;
                    previous_failure = Some(pf);
                }
                RetryDecision::GiveUp => {
                    return Ok(BeadResult::Clarified {
                        last_error: previous_failure.unwrap_or_default(),
                    });
                }
            },
        }
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use loom_core::bd::{Bead, Label};
    use loom_core::identifier::BeadId;
    use std::collections::VecDeque;

    /// Capturing fake controller. Drives [`run_loop`] without touching real
    /// bd / agent / check binaries.
    #[derive(Default)]
    struct FakeController {
        ready_queue: VecDeque<Bead>,
        agent_outcomes: VecDeque<AgentOutcome>,
        run_calls: Vec<(BeadId, Option<String>)>,
        closed: Vec<BeadId>,
        clarified: Vec<BeadId>,
        check_calls: u32,
    }

    impl AgentLoopController for FakeController {
        async fn next_ready_bead(&mut self) -> Result<Option<Bead>, RunError> {
            Ok(self.ready_queue.pop_front())
        }

        async fn run_bead(
            &mut self,
            bead: &Bead,
            previous_failure: Option<String>,
        ) -> Result<AgentOutcome, RunError> {
            self.run_calls.push((bead.id.clone(), previous_failure));
            Ok(self
                .agent_outcomes
                .pop_front()
                .unwrap_or(AgentOutcome::Success))
        }

        async fn close_bead(&mut self, bead: &BeadId) -> Result<(), RunError> {
            self.closed.push(bead.clone());
            Ok(())
        }

        async fn apply_clarify(&mut self, bead: &BeadId) -> Result<(), RunError> {
            self.clarified.push(bead.clone());
            Ok(())
        }

        async fn exec_check(&mut self) -> Result<(), RunError> {
            self.check_calls += 1;
            Ok(())
        }
    }

    fn bead(id: &str, labels: &[&str]) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: format!("title for {id}"),
            description: "desc".into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: labels.iter().map(|s| Label::new(*s)).collect(),
        }
    }

    #[tokio::test]
    async fn once_mode_processes_single_bead() -> Result<(), RunError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        c.ready_queue.push_back(bead("wx-2", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, RunMode::Once, RetryPolicy::default()).await?;

        assert_eq!(summary.beads_processed, 1);
        assert_eq!(c.closed, vec![BeadId::new("wx-1").expect("valid")]);
        assert_eq!(c.run_calls.len(), 1);
        assert!(c.clarified.is_empty());
        assert_eq!(c.check_calls, 0, "once mode never execs check");
        // Second bead remains in the queue; run_loop did not pull it.
        assert_eq!(c.ready_queue.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn continuous_loops_until_molecule_complete() -> Result<(), RunError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        c.ready_queue.push_back(bead("wx-2", &[]));
        c.ready_queue.push_back(bead("wx-3", &[]));
        for _ in 0..3 {
            c.agent_outcomes.push_back(AgentOutcome::Success);
        }

        let summary = run_loop(&mut c, RunMode::Continuous, RetryPolicy::default()).await?;

        assert_eq!(summary.beads_processed, 3);
        assert_eq!(
            c.closed,
            vec![
                BeadId::new("wx-1").expect("valid"),
                BeadId::new("wx-2").expect("valid"),
                BeadId::new("wx-3").expect("valid"),
            ]
        );
        assert!(summary.molecule_complete);
        assert!(summary.execed_check);
        Ok(())
    }

    #[tokio::test]
    async fn continuous_execs_check_on_molecule_complete() -> Result<(), RunError> {
        // Empty ready queue → first iteration sees None → exec check.
        let mut c = FakeController::default();
        let summary = run_loop(&mut c, RunMode::Continuous, RetryPolicy::default()).await?;
        assert_eq!(summary.beads_processed, 0);
        assert!(summary.molecule_complete);
        assert!(summary.execed_check);
        assert_eq!(c.check_calls, 1);
        Ok(())
    }

    #[tokio::test]
    async fn once_mode_does_not_exec_check_on_empty_queue() -> Result<(), RunError> {
        let mut c = FakeController::default();
        let summary = run_loop(&mut c, RunMode::Once, RetryPolicy::default()).await?;
        assert!(summary.molecule_complete);
        assert!(!summary.execed_check, "once mode never execs check");
        assert_eq!(c.check_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn failed_bead_retries_with_previous_failure_then_clarifies() -> Result<(), RunError> {
        // max_retries = 2 → attempts = initial + 2 retries = 3 failures triggers clarify.
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        for i in 0..3 {
            c.agent_outcomes.push_back(AgentOutcome::Failure {
                error: format!("err-{i}"),
            });
        }

        let summary = run_loop(&mut c, RunMode::Once, RetryPolicy { max_retries: 2 }).await?;

        assert_eq!(c.run_calls.len(), 3, "initial + 2 retries");
        // Attempt 1 has no previous_failure.
        assert_eq!(c.run_calls[0].1, None);
        // Attempts 2 and 3 carry the prior error verbatim.
        assert_eq!(c.run_calls[1].1.as_deref(), Some("err-0"));
        assert_eq!(c.run_calls[2].1.as_deref(), Some("err-1"));

        assert!(c.closed.is_empty());
        assert_eq!(c.clarified, vec![BeadId::new("wx-1").expect("valid")]);
        assert_eq!(summary.beads_clarified, 1);
        Ok(())
    }

    #[tokio::test]
    async fn retry_succeeds_within_budget_and_closes() -> Result<(), RunError> {
        let mut c = FakeController::default();
        c.ready_queue.push_back(bead("wx-1", &[]));
        c.agent_outcomes.push_back(AgentOutcome::Failure {
            error: "boom".into(),
        });
        c.agent_outcomes.push_back(AgentOutcome::Success);

        let summary = run_loop(&mut c, RunMode::Once, RetryPolicy { max_retries: 2 }).await?;

        assert_eq!(c.run_calls.len(), 2);
        assert_eq!(c.run_calls[1].1.as_deref(), Some("boom"));
        assert_eq!(c.closed, vec![BeadId::new("wx-1").expect("valid")]);
        assert!(c.clarified.is_empty());
        assert_eq!(summary.beads_clarified, 0);
        Ok(())
    }
}
