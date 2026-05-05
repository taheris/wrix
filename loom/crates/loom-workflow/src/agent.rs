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

use std::time::Duration;

use loom_core::agent::{
    Active, AgentBackend, AgentEvent, AgentSession, DEFAULT_STALL_WARN_SECS, Idle, ProtocolError,
    SessionOutcome, SpawnConfig,
};
use loom_core::clock::{Clock, SystemClock};
use loom_core::logging::{BeadOutcome, LogSink};
use tracing::{info, warn};

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
    let stall_window = config
        .stall_warn_interval
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_STALL_WARN_SECS));
    let clock = SystemClock::new();
    let session = B::spawn(config).await?;
    info!(
        prompt_chars = config.initial_prompt.chars().count(),
        stall_warn_secs = stall_window.as_secs(),
        "agent spawned; sending initial prompt",
    );
    let mut session =
        prompt_with_stall_warn(session, &config.initial_prompt, stall_window, &clock).await?;
    info!("prompt sent; awaiting agent events");
    loop {
        let next = next_event_with_stall_warn(&mut session, stall_window, &clock).await;
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
        info!(event = %summarize_event(&event), "agent event");
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
            if let Err(e) = B::after_session_complete(session, config).await {
                warn!(error = %e, "backend shutdown hook failed");
            }
            finish_sink(sink, outcome);
            return Ok(SessionOutcome {
                exit_code,
                cost_usd,
            });
        }
    }
}

/// Drive [`AgentSession::prompt`] to completion while emitting a periodic
/// `warn!` every `stall_window` that the write hasn't returned. Closes the
/// visibility gap between `B::spawn` returning and the first agent event:
/// for the claude backend that window is the container starting up and
/// claude opening stdin, and a slow consumer can leave the pipe write
/// blocked with no log output. `stall_window == Duration::ZERO` disables
/// the watchdog (used by tests).
async fn prompt_with_stall_warn(
    session: AgentSession<Idle>,
    msg: &str,
    stall_window: Duration,
    clock: &dyn Clock,
) -> Result<AgentSession<Active>, ProtocolError> {
    let fut = session.prompt(msg);
    if stall_window.is_zero() {
        return fut.await;
    }
    tokio::pin!(fut);
    loop {
        let sleep = clock.sleep(stall_window);
        tokio::select! {
            biased;
            result = &mut fut => return result,
            () = sleep => warn!(
                stall_secs = stall_window.as_secs(),
                "still writing initial prompt to agent — agent stdin not draining yet",
            ),
        }
    }
}

/// Poll [`AgentSession::next_event`] while emitting a periodic `warn!`
/// every `stall_window` of silence. The warning does not abort the run —
/// claude can legitimately think for minutes — but it ends the silent
/// stare at the terminal so the operator can decide whether to intervene.
///
/// `stall_window == Duration::ZERO` disables the watchdog explicitly. A
/// fresh `clock.sleep(stall_window)` is created on every loop iteration so
/// each warning resets the silence window.
async fn next_event_with_stall_warn(
    session: &mut AgentSession<Active>,
    stall_window: Duration,
    clock: &dyn Clock,
) -> Result<Option<AgentEvent>, ProtocolError> {
    let next = session.next_event();
    if stall_window.is_zero() {
        return next.await;
    }
    tokio::pin!(next);
    loop {
        let sleep = clock.sleep(stall_window);
        tokio::select! {
            biased;
            result = &mut next => return result,
            () = sleep => warn!(
                stall_secs = stall_window.as_secs(),
                "no agent event for stall window — still waiting",
            ),
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

fn summarize_event(event: &AgentEvent) -> String {
    match event {
        AgentEvent::MessageDelta { text } => {
            format!("message_delta ({} chars)", text.chars().count())
        }
        AgentEvent::ToolCall { id, tool, .. } => format!("tool_call {tool} (id={id})"),
        AgentEvent::ToolResult {
            id,
            output,
            is_error,
        } => format!(
            "tool_result (id={id}, is_error={is_error}, {} chars)",
            output.chars().count(),
        ),
        AgentEvent::TurnEnd => "turn_end".to_string(),
        AgentEvent::SessionComplete {
            exit_code,
            cost_usd,
        } => format!("session_complete (exit_code={exit_code}, cost_usd={cost_usd:?})",),
        AgentEvent::CompactionStart { reason } => format!("compaction_start ({reason:?})"),
        AgentEvent::CompactionEnd { aborted } => format!("compaction_end (aborted={aborted})"),
        AgentEvent::Error { message } => format!("error: {message}"),
    }
}
