use loom_core::agent::{AgentBackend, AgentSession, Idle, ProtocolError, SpawnConfig};

/// Zero-sized marker for the Claude Code stream-json backend.
///
/// Per the spec's static-dispatch design, all runtime state lives in the
/// spawned [`AgentSession`] and the [`SpawnConfig`] passed to
/// [`AgentBackend::spawn`]. The spawn body launches `claude --print
/// --input-format stream-json --output-format stream-json` (with
/// `--permission-prompt-tool stdio` so tool permissions flow over the same
/// pipe) and is implemented in wx-pkht8.8.
pub struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    async fn spawn(_config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
        // The real spawn — `wrapix run-bead` invocation, stream-json wiring,
        // shutdown watchdog, re-pin file emission — lands in wx-pkht8.8. The
        // skeleton fails closed so any code that reaches into the backend
        // before that point surfaces a clear protocol-level error rather
        // than a half-initialised session.
        Err(ProtocolError::Unsupported)
    }
}
