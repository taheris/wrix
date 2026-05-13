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

use loom_driver::agent::{
    Active, AgentBackend, AgentEvent, AgentSession, DEFAULT_STALL_WARN_SECS, Idle, ProtocolError,
    SessionOutcome, SpawnConfig,
};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::logging::{BeadOutcome, LogSink};
use tracing::{info, warn};

use crate::run::SessionResult;

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
    sink: Option<LogSink>,
    text_capture: Option<&mut String>,
) -> Result<SessionOutcome, ProtocolError> {
    match run_agent_classified::<B>(config, sink, text_capture, None).await {
        SessionResult::Complete(outcome) => Ok(outcome),
        // Callers that only accept the legacy `Result` shape (todo, plan,
        // msg, batch dispatch) treat both infra phases as a single failure
        // surface. The run-loop dispatch path in `main.rs` calls
        // `run_agent_classified` directly so it can preserve the
        // preflight/mid-session distinction the verdict gate relies on.
        SessionResult::PreflightFailed { error } | SessionResult::MidSessionFailed { error } => {
            Err(ProtocolError::Io(std::io::Error::other(error)))
        }
    }
}

/// Same as [`run_agent`] but preserves the preflight vs mid-session
/// distinction in its return type. Used by the `loom run` driver so the
/// verdict gate can route pre-flight failures to `loom:blocked` cause
/// `infra-preflight` immediately and grant mid-session failures one
/// driver-memory retry per `loom run`.
///
/// `envelope_builder` is the R1 (wx-cqzxh) plumbing: when `Some`, every
/// event emitted by the parser has its placeholder envelope replaced
/// with a real per-spawn envelope (monotonic `seq`, real `bead_id`,
/// real wall-clock `ts_ms`). When `None`, events flow with whatever
/// the parser produced (currently `EventEnvelope::default()` per G1's
/// interim shape); used by tests and the legacy `run_agent` wrapper.
pub async fn run_agent_classified<B: AgentBackend>(
    config: &SpawnConfig,
    mut sink: Option<LogSink>,
    mut text_capture: Option<&mut String>,
    mut envelope_builder: Option<loom_events::EnvelopeBuilder>,
) -> SessionResult {
    let stall_window = config
        .stall_warn_interval
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_STALL_WARN_SECS));
    let clock = SystemClock::new();
    let session = match B::spawn(config).await {
        Ok(session) => session,
        Err(err) => {
            // Spec: "Pre-flight failures (image load, container start) →
            // exit immediately as blocked with cause infra-preflight —
            // there is no agent output to evaluate." `B::spawn` is the
            // boundary that owns image load + container construction; any
            // failure here lands in the preflight bucket regardless of
            // backend.
            warn!(error = %err, "agent spawn failed before session became live");
            finish_sink(sink, BeadOutcome::Failed);
            return SessionResult::PreflightFailed {
                error: err.to_string(),
            };
        }
    };
    info!(
        prompt_chars = config.initial_prompt.chars().count(),
        stall_warn_secs = stall_window.as_secs(),
        "agent spawned; sending initial prompt",
    );
    let mut session =
        match prompt_with_stall_warn(session, &config.initial_prompt, stall_window, &clock).await {
            Ok(s) => s,
            Err(err) => {
                finish_sink(sink, BeadOutcome::Failed);
                return SessionResult::MidSessionFailed {
                    error: err.to_string(),
                };
            }
        };
    info!("prompt sent; awaiting agent events");
    loop {
        let next = next_event_with_stall_warn(&mut session, stall_window, &clock).await;
        let mut event = match next {
            Ok(Some(event)) => event,
            Ok(None) => {
                finish_sink(sink, BeadOutcome::Failed);
                return SessionResult::MidSessionFailed {
                    error: ProtocolError::UnexpectedEof.to_string(),
                };
            }
            Err(err) => {
                finish_sink(sink, BeadOutcome::Failed);
                return SessionResult::MidSessionFailed {
                    error: err.to_string(),
                };
            }
        };
        // R1 (wx-cqzxh): stamp the real per-spawn envelope onto each
        // event before any consumer sees it. Parser produced a
        // placeholder; this is where the live bead context lands.
        if let Some(builder) = envelope_builder.as_mut() {
            *event.envelope_mut() = builder.build();
        }
        info!(event = %summarize_event(&event), "agent event");
        if let AgentEvent::TextDelta { text, .. } = &event
            && let Some(buf) = text_capture.as_deref_mut()
        {
            buf.push_str(text);
        }
        if let Some(s) = sink.as_mut()
            && let Err(e) = s.emit(&event)
        {
            warn!(error = %e, "log sink emit failed");
            finish_sink(sink, BeadOutcome::Failed);
            return SessionResult::MidSessionFailed {
                error: format!("log sink emit failed: {e}"),
            };
        }
        if matches!(event, AgentEvent::CompactionStart { .. })
            && let Err(e) = B::on_compaction_start(&mut session, config).await
        {
            warn!(error = %e, "backend compaction handler failed");
            finish_sink(sink, BeadOutcome::Failed);
            return SessionResult::MidSessionFailed {
                error: e.to_string(),
            };
        }
        if let AgentEvent::SessionComplete {
            exit_code,
            cost_usd,
            ..
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
            return SessionResult::Complete(SessionOutcome {
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
        AgentEvent::AgentStart { title, profile, .. } => {
            format!("agent_start ({title}, profile={profile})")
        }
        AgentEvent::TextDelta { text, .. } => {
            format!("message_delta ({} chars)", text.chars().count())
        }
        AgentEvent::ToolCall { id, tool, .. } => format!("tool_call {tool} (id={id})"),
        AgentEvent::ToolResult {
            id,
            output,
            is_error,
            ..
        } => format!(
            "tool_result (id={id}, is_error={is_error}, {} chars)",
            output.chars().count(),
        ),
        AgentEvent::TurnEnd { .. } => "turn_end".to_string(),
        AgentEvent::SessionComplete {
            exit_code,
            cost_usd,
            ..
        } => format!("session_complete (exit_code={exit_code}, cost_usd={cost_usd:?})",),
        AgentEvent::CompactionStart { reason, .. } => format!("compaction_start ({reason:?})"),
        AgentEvent::CompactionEnd { aborted, .. } => {
            format!("compaction_end (aborted={aborted})")
        }
        AgentEvent::Error { message, .. } => format!("error: {message}"),
        AgentEvent::AgentEnd { .. } => "agent_end".to_string(),
        AgentEvent::TurnStart { .. } => "turn_start".to_string(),
        AgentEvent::TextEnd { .. } => "text_end".to_string(),
        AgentEvent::ThinkingDelta { text, .. } => {
            format!("thinking_delta ({} chars)", text.chars().count())
        }
        AgentEvent::ThinkingEnd { .. } => "thinking_end".to_string(),
        AgentEvent::ToolcallDelta { id, delta, .. } => {
            format!("toolcall_delta (id={id}, {} chars)", delta.chars().count())
        }
        AgentEvent::ToolProgress { id, text, .. } => {
            format!("tool_progress (id={id}, {} chars)", text.chars().count())
        }
        AgentEvent::AutoRetry {
            attempt,
            max_attempts,
            ..
        } => format!("auto_retry (attempt={attempt}/{max_attempts})"),
        AgentEvent::DriverEvent {
            driver_kind,
            summary,
            ..
        } => format!("driver_event {driver_kind}: {summary}"),
    }
}
