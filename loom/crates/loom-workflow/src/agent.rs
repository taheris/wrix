//! Backend-agnostic session driver.
//!
//! [`run_agent`] is the single function the workflow modules call to drive a
//! [`SpawnConfig`] through any [`AgentBackend`]. The binary crate
//! monomorphizes one copy per concrete backend (`run_agent::<PiBackend>` and
//! `run_agent::<ClaudeBackend>`) inside a `dispatch` match and hands the
//! resulting closure into the workflow modules — keeping
//! [`run_agent`]'s `<B: AgentBackend>` parameter the only place the workflow
//! is generic over the backend.
//!
//! Every [`AgentEvent`] consumed from the typestate session is tee'd into an
//! optional [`LogSink`] (when one is supplied) so the spec contract — "the
//! terminal renderer consumes the same `AgentEvent` stream that's written to
//! disk" — is enforced through a single emission point. Sink lifecycle
//! ownership lives here: passing `Some(sink)` consumes it, and `run_agent`
//! calls [`LogSink::finish`] before returning so callers can rely on the
//! file being closed and flushed regardless of the exit path.

use loom_core::agent::{AgentBackend, AgentEvent, ProtocolError, SessionOutcome, SpawnConfig};
use loom_core::logging::{BeadOutcome, LogSink};
use tracing::{trace, warn};

/// Drive `B` through one full session: spawn, prompt, then consume events
/// until `SessionComplete` arrives. Returns the resulting [`SessionOutcome`]
/// (exit code + cost, when surfaced by the backend).
///
/// When `sink` is `Some`, every observed event is emitted into the sink in
/// arrival order — including the terminal `SessionComplete` — and the sink
/// is finished with the appropriate [`BeadOutcome`] before this function
/// returns. Sink-write failures map to [`ProtocolError::Io`] so the caller
/// surfaces a single error type regardless of which subsystem failed.
///
/// `UnexpectedEof` is returned if the agent process closes its stdout
/// without emitting a terminal event — this signals the caller that the
/// session ended abnormally and the outcome is not trustworthy.
pub async fn run_agent<B: AgentBackend>(
    config: &SpawnConfig,
    mut sink: Option<LogSink>,
) -> Result<SessionOutcome, ProtocolError> {
    let session = B::spawn(config).await?;
    let mut session = session.prompt(&config.initial_prompt).await?;
    loop {
        let next = session.next_event().await;
        let event = match next {
            Ok(Some(event)) => event,
            Ok(None) => {
                finish_sink(sink, BeadOutcome::Failed);
                return Err(ProtocolError::UnexpectedEof);
            }
            Err(err) => {
                finish_sink(sink, BeadOutcome::Failed);
                return Err(err);
            }
        };
        trace!(?event, "agent event");
        if let Some(s) = sink.as_mut()
            && let Err(e) = s.emit(&event)
        {
            warn!(error = %e, "log sink emit failed");
            finish_sink(sink, BeadOutcome::Failed);
            return Err(ProtocolError::Io(std::io::Error::other(e.to_string())));
        }
        if matches!(event, AgentEvent::CompactionStart { .. })
            && let Err(e) = B::on_compaction_start(&mut session, config).await
        {
            warn!(error = %e, "backend compaction handler failed");
            finish_sink(sink, BeadOutcome::Failed);
            return Err(e);
        }
        if let AgentEvent::SessionComplete {
            exit_code,
            cost_usd,
        } = event
        {
            let outcome = if exit_code == 0 {
                BeadOutcome::Done
            } else {
                BeadOutcome::Failed
            };
            finish_sink(sink, outcome);
            return Ok(SessionOutcome {
                exit_code,
                cost_usd,
            });
        }
    }
}

fn finish_sink(sink: Option<LogSink>, outcome: BeadOutcome) {
    if let Some(mut s) = sink
        && let Err(e) = s.finish(outcome)
    {
        warn!(error = %e, "log sink finish failed");
    }
}
