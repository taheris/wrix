use std::collections::BTreeMap;

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agent::AgentKind;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Backend to use when no per-phase override applies.
    pub default: String,

    /// Per-phase overrides keyed by phase name (`run`, `check`, `todo`, ...).
    /// Captured via `flatten` so the TOML keeps its `[agent.<phase>]` shape.
    #[serde(flatten)]
    pub overrides: BTreeMap<String, PhaseOverride>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default: "claude".to_string(),
            overrides: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct PhaseOverride {
    pub backend: Option<String>,
    pub provider: Option<String>,
    pub model_id: Option<String>,
}

/// Workflow phase that resolves an [`AgentSelection`] from config.
///
/// `[agent.<phase>]` table keys in TOML correspond one-to-one with this enum's
/// snake_case variants: `plan`, `todo`, `run`, `check`, `msg`. The enum is the
/// closed dispatch surface used by `LoomConfig::agent_for`; the BTreeMap that
/// backs `[agent.*]` overrides remains string-keyed so unknown TOML keys
/// surface at lookup time rather than at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Plan,
    Todo,
    Run,
    Check,
    Msg,
}

impl Phase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Phase::Plan => "plan",
            Phase::Todo => "todo",
            Phase::Run => "run",
            Phase::Check => "check",
            Phase::Msg => "msg",
        }
    }
}

/// Claude-backend-specific runtime settings surfaced through
/// [`AgentSelection::claude_settings`] when the resolved backend is
/// [`AgentKind::Claude`]. Pi has no analog (no host-side permission flow).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSettings {
    /// Tool names denied at host-side `control_request` time. Sourced from
    /// `[security] denied_tools`.
    pub denied_tools: Vec<String>,
    /// Seconds to wait for clean exit after `result` before SIGTERM. Sourced
    /// from `[claude] post_result_grace_secs`.
    pub post_result_grace_secs: u32,
}

/// Backend + per-phase model selection resolved by [`super::LoomConfig::agent_for`].
///
/// `kind` carries the selected backend (after applying any phase override on
/// top of `[agent] default`). `provider` / `model_id` hold the per-phase
/// model override for the pi backend (`set_model { provider, modelId }`).
/// `claude_settings` is populated only when `kind == Claude` so call sites
/// can wire the post-result grace period and denied-tools list without a
/// second config lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSelection {
    pub kind: AgentKind,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    pub claude_settings: Option<ClaudeSettings>,
}

#[derive(Debug, Display, Error, PartialEq, Eq)]
pub enum AgentSelectionError {
    /// unknown agent backend `{name}` in config (expected `claude` or `pi`)
    UnknownBackend { name: String },
}

/// Convert a backend name string (from TOML `default` or `[agent.<phase>] backend`)
/// into the typed [`AgentKind`].
pub fn parse_backend_name(name: &str) -> Result<AgentKind, AgentSelectionError> {
    match name {
        "claude" => Ok(AgentKind::Claude),
        "pi" => Ok(AgentKind::Pi),
        other => Err(AgentSelectionError::UnknownBackend {
            name: other.to_string(),
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn phase_round_trips_through_serde() {
        for (phase, expected) in [
            (Phase::Plan, "plan"),
            (Phase::Todo, "todo"),
            (Phase::Run, "run"),
            (Phase::Check, "check"),
            (Phase::Msg, "msg"),
        ] {
            assert_eq!(
                serde_json::to_string(&phase).unwrap(),
                format!("\"{expected}\"")
            );
            let back: Phase = serde_json::from_str(&format!("\"{expected}\"")).unwrap();
            assert_eq!(back, phase);
            assert_eq!(phase.as_str(), expected);
        }
    }

    #[test]
    fn parse_backend_name_accepts_claude_and_pi() {
        assert_eq!(parse_backend_name("claude").unwrap(), AgentKind::Claude);
        assert_eq!(parse_backend_name("pi").unwrap(), AgentKind::Pi);
    }

    #[test]
    fn parse_backend_name_rejects_unknown() {
        match parse_backend_name("gpt") {
            Err(AgentSelectionError::UnknownBackend { name }) => assert_eq!(name, "gpt"),
            other => panic!("expected UnknownBackend, got {other:?}"),
        }
    }
}
