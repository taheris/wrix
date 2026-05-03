use loom_core::agent::{AgentBackend, AgentSession, Idle, ProtocolError, SpawnConfig};

/// Zero-sized marker for the pi-mono RPC backend.
///
/// Per the spec's static-dispatch design, all runtime state lives in the
/// spawned [`AgentSession`] and the [`SpawnConfig`] passed to
/// [`AgentBackend::spawn`] — the backend itself carries no fields. The type
/// parameter alone is what dispatches `<B: AgentBackend>` call sites in
/// `loom-workflow` (`run_agent::<PiBackend>(..)` versus
/// `run_agent::<ClaudeBackend>(..)`).
pub struct PiBackend;

impl AgentBackend for PiBackend {
    async fn spawn(_config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
        // The real spawn — `wrapix run-bead --spawn-config <file> --stdio`,
        // `get_commands` probe, NDJSON wiring — lands in wx-pkht8.6. The
        // skeleton fails closed so any code that reaches into the backend
        // before that point surfaces a clear protocol-level error rather
        // than a half-initialised session.
        Err(ProtocolError::Unsupported)
    }
}
