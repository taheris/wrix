//! Backend-agnostic driver for `loom todo`.
//!
//! [`run`] is the entry point the binary calls to execute one todo session.
//! The function takes a controller (which owns spec/state/bd interactions)
//! and a `spawn` closure (which the binary builds out of the per-phase
//! `dispatch` function). Keeping the spawn step generic over a closure
//! preserves static dispatch — the binary monomorphizes `run_agent::<B>` per
//! concrete backend inside its `dispatch` match, and this driver never sees
//! the backend type.

use loom_core::agent::{ProtocolError, SessionOutcome, SpawnConfig};

use super::error::TodoError;

/// Side-effect surface the [`run`] driver depends on.
///
/// The trait is deliberately shallow — it owns the spec read, prompt build,
/// and outcome recording but leaves agent invocation to the caller-provided
/// `spawn` closure. The split keeps backend selection out of the controller
/// (the binary's `dispatch` function owns that) while letting tests substitute
/// fakes for the bd/state/git surface.
pub trait TodoController: Send {
    /// Compute the per-bead [`SpawnConfig`] for the upcoming todo session —
    /// resolve the tier decision, render the prompt, build the env allowlist.
    fn build_spawn_config(
        &mut self,
    ) -> impl std::future::Future<Output = Result<SpawnConfig, TodoError>> + Send;

    /// Persist the agent outcome — write per-spec cursors, commit the spec
    /// file, etc. Called once after the agent session completes.
    fn record_outcome(
        &mut self,
        outcome: &SessionOutcome,
    ) -> impl std::future::Future<Output = Result<(), TodoError>> + Send;
}

/// Summary of one [`run`] invocation surfaced to the binary so it can print
/// a human-readable line and exit appropriately.
#[derive(Debug, Clone, PartialEq)]
pub struct TodoSummary {
    pub exit_code: i32,
    pub cost_usd: Option<f64>,
}

/// Drive one `loom todo` session: build the spawn config, hand it to the
/// caller-provided agent dispatcher, then record the outcome.
///
/// `spawn` is the per-phase backend dispatcher closure. The binary builds it
/// from `dispatch(Phase::Todo, &config, _)` so the workflow stays
/// backend-agnostic. Errors from the closure are surfaced as
/// [`TodoError::Protocol`] — the binary maps them to the user-visible exit
/// status.
pub async fn run<C, S, F>(controller: &mut C, spawn: S) -> Result<TodoSummary, TodoError>
where
    C: TodoController + ?Sized,
    S: FnOnce(SpawnConfig) -> F,
    F: std::future::Future<Output = Result<SessionOutcome, ProtocolError>>,
{
    let cfg = controller.build_spawn_config().await?;
    let outcome = spawn(cfg).await?;
    controller.record_outcome(&outcome).await?;
    Ok(TodoSummary {
        exit_code: outcome.exit_code,
        cost_usd: outcome.cost_usd,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use loom_core::agent::RePinContent;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct FakeController {
        recorded: AtomicU32,
        last_exit: std::sync::Mutex<Option<i32>>,
    }

    impl FakeController {
        fn new() -> Self {
            Self {
                recorded: AtomicU32::new(0),
                last_exit: std::sync::Mutex::new(None),
            }
        }
    }

    impl TodoController for FakeController {
        async fn build_spawn_config(&mut self) -> Result<SpawnConfig, TodoError> {
            Ok(SpawnConfig {
                image: "wrapix-base:latest".into(),
                workspace: PathBuf::from("/workspace"),
                env: vec![],
                initial_prompt: "todo prompt".into(),
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
            self.recorded.fetch_add(1, Ordering::SeqCst);
            *self.last_exit.lock().unwrap() = Some(outcome.exit_code);
            Ok(())
        }
    }

    #[tokio::test]
    async fn run_threads_spawn_outcome_through_controller() {
        let mut controller = FakeController::new();
        let summary = run(&mut controller, |cfg: SpawnConfig| {
            assert_eq!(cfg.initial_prompt, "todo prompt");
            async move {
                Ok(SessionOutcome {
                    exit_code: 0,
                    cost_usd: Some(0.42),
                })
            }
        })
        .await
        .expect("run ok");
        assert_eq!(summary.exit_code, 0);
        assert_eq!(summary.cost_usd, Some(0.42));
        assert_eq!(controller.recorded.load(Ordering::SeqCst), 1);
        assert_eq!(*controller.last_exit.lock().unwrap(), Some(0));
    }

    #[tokio::test]
    async fn spawn_error_is_surfaced_as_protocol_error() {
        let mut controller = FakeController::new();
        let result = run(&mut controller, |_cfg: SpawnConfig| async {
            Err(ProtocolError::Unsupported)
        })
        .await;
        match result {
            Err(TodoError::Protocol(ProtocolError::Unsupported)) => {}
            other => panic!("expected Protocol(Unsupported), got {other:?}"),
        }
        // Outcome never recorded when the agent fails.
        assert_eq!(controller.recorded.load(Ordering::SeqCst), 0);
    }
}
