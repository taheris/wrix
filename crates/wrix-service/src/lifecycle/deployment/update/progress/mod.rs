//! Observational update progress never drives lifecycle decisions.

use std::{
    sync::mpsc::{self, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Stage {
    WaitingForUpdateLock,
    RealizingImage,
    RecoveringGatewayRetirement,
    PreparingController,
    PreparingPools,
    CheckingOwnership,
    PreparingGateway,
    DrainingBackend,
    ReleasingBackend,
    Checkpointing,
    StartingBackend,
    CheckingReadiness,
    CheckingPools,
    Finalizing,
}

pub(super) struct Reporter(Sender<Stage>);

impl Reporter {
    pub(super) fn stage(&self, stage: Stage) {
        if let Err(error) = self.0.send(stage) {
            tracing::warn!(%error, "service update progress reporter unavailable");
        }
    }
}

pub(super) fn run<T>(operation: impl FnOnce(&Reporter) -> T) -> T {
    let dispatch = tracing::dispatcher::get_default(Clone::clone);
    observe(operation, Duration::from_secs(15), move |stage, elapsed| {
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(target: "wrix::service::update", ?stage, elapsed_secs = elapsed.as_secs(), "Service update");
        });
    })
}

fn observe<T>(
    operation: impl FnOnce(&Reporter) -> T,
    interval: Duration,
    mut report: impl FnMut(Stage, Duration) + Send,
) -> T {
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        scope.spawn(move || {
            let mut stage = Stage::WaitingForUpdateLock;
            let mut started = Instant::now();
            report(stage, Duration::ZERO);
            loop {
                match receiver.recv_timeout(interval) {
                    Ok(next) => {
                        stage = next;
                        started = Instant::now();
                        report(stage, Duration::ZERO);
                    }
                    Err(RecvTimeoutError::Timeout) => report(stage, started.elapsed()),
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        operation(&Reporter(sender))
    })
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn reports_stage_changes_without_changing_the_result() {
        let mut events = Vec::new();
        let result: Result<(), &str> = observe(
            |progress| {
                progress.stage(Stage::RealizingImage);
                progress.stage(Stage::CheckingReadiness);
                Err("retained failure")
            },
            Duration::from_secs(15),
            |stage, elapsed| events.push((stage, elapsed)),
        );
        assert_eq!(result, Err("retained failure"));
        assert_eq!(
            events,
            [
                (Stage::WaitingForUpdateLock, Duration::ZERO),
                (Stage::RealizingImage, Duration::ZERO),
                (Stage::CheckingReadiness, Duration::ZERO),
            ]
        );
    }

    #[test]
    fn reports_elapsed_time_while_an_operation_is_blocked() {
        let (sender, receiver) = mpsc::channel();
        let mut sender = Some(sender);
        observe(
            |progress| {
                progress.stage(Stage::RealizingImage);
                let elapsed = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(elapsed > Duration::ZERO);
            },
            Duration::from_millis(1),
            |stage, elapsed| {
                if stage == Stage::RealizingImage
                    && elapsed > Duration::ZERO
                    && let Some(sender) = sender.take()
                {
                    sender.send(elapsed).unwrap();
                }
            },
        );
    }
}
