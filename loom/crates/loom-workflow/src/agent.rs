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
//! The function consumes events from the typestate session until it observes
//! `AgentEvent::SessionComplete`, draining and (currently) discarding
//! intermediate events. Subsequent issues will tee the event stream into the
//! per-bead NDJSON log + terminal renderer; the surface here stays fixed so
//! that wiring is local.

use loom_core::agent::{AgentBackend, AgentEvent, ProtocolError, SessionOutcome, SpawnConfig};
use tracing::trace;

/// Drive `B` through one full session: spawn, prompt, then consume events
/// until `SessionComplete` arrives. Returns the resulting [`SessionOutcome`]
/// (exit code + cost, when surfaced by the backend).
///
/// `UnexpectedEof` is returned if the agent process closes its stdout
/// without emitting a terminal event — this signals the caller that the
/// session ended abnormally and the outcome is not trustworthy.
pub async fn run_agent<B: AgentBackend>(
    config: &SpawnConfig,
) -> Result<SessionOutcome, ProtocolError> {
    let session = B::spawn(config).await?;
    let mut session = session.prompt(&config.initial_prompt).await?;
    loop {
        match session.next_event().await? {
            Some(AgentEvent::SessionComplete {
                exit_code,
                cost_usd,
            }) => {
                return Ok(SessionOutcome {
                    exit_code,
                    cost_usd,
                });
            }
            Some(event) => {
                trace!(?event, "agent event");
            }
            None => return Err(ProtocolError::UnexpectedEof),
        }
    }
}
