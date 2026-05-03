use std::collections::BTreeMap;

use serde::Deserialize;

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
