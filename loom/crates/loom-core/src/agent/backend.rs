use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::error::ProtocolError;
use super::repin::RePinContent;
use super::session::{AgentSession, Idle};

/// Configuration `loom` hands to `wrapix run-bead` describing how to launch
/// the per-bead container and what initial agent state to install.
///
/// Serialized to a JSON file (`/tmp/loom-<id>.json`) and read back by
/// `wrapix run-bead --spawn-config <file>` — this is the single stable
/// boundary between loom and the wrapper. `env` is an explicit allowlist;
/// the wrapper never inherits the host environment wholesale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnConfig {
    pub image: String,
    pub workspace: PathBuf,
    pub env: Vec<(String, String)>,
    pub initial_prompt: String,
    pub agent_args: Vec<String>,
    pub repin: RePinContent,
    /// Optional post-spawn model override consumed by the host-side backend
    /// (currently only [`PiBackend`](crate::agent::AgentBackend) — claude
    /// receives its model via CLI flags). When present, the pi backend sends
    /// a `set_model` RPC after the startup probe; failure is hard-fail.
    /// Skipped during serialization when `None` so the wrapper's input JSON
    /// remains identical to existing fixtures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
}

/// Per-session model override: pi RPC's `set_model { provider, modelId }`.
///
/// Lives on [`SpawnConfig`] rather than a backend-specific config object so
/// the [`AgentBackend::spawn`] trait surface stays a single-argument call.
/// The wrapper ignores this field; it is consumed only by host-side backends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSelection {
    pub provider: String,
    pub model_id: String,
}

/// Outcome of a completed agent session — what the workflow engine receives
/// after the session reaches `SessionComplete`.
#[derive(Debug, Clone)]
pub struct SessionOutcome {
    pub exit_code: i32,
    pub cost_usd: Option<f64>,
}

/// Backend abstraction: spawn a session and return it in the `Idle` state.
///
/// The trait surface is deliberately minimal — process lifecycle only.
/// Conversation driving (prompt, steer, abort, event streaming) lives on
/// [`AgentSession`] so both backends share one concrete session type.
///
/// `async fn` in traits is used directly (no `async-trait`) — backends are
/// zero-sized types dispatched via a type parameter (`<B: AgentBackend>`),
/// so the compiler monomorphizes per concrete backend at each call site.
/// The desugared `impl Future + Send` form pins the auto-trait bound so the
/// returned future can cross task boundaries in `loom-workflow`.
pub trait AgentBackend: Send + Sync {
    fn spawn(
        config: &SpawnConfig,
    ) -> impl std::future::Future<Output = Result<AgentSession<Idle>, ProtocolError>> + Send;
}
