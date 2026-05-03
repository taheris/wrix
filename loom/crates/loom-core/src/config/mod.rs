//! Loom configuration loaded from `.wrapix/loom/config.toml`.
//!
//! Parsed natively via the `toml` crate into a typed [`LoomConfig`]. Every
//! field carries `#[serde(default)]` so a missing or empty file yields
//! defaults that match Ralph's `.wrapix/ralph/config.nix`, letting users
//! transition without writing a Loom config.

mod agent;
mod beads;
mod claude;
mod error;
mod exit_signals;
mod logs;
mod loop_config;
mod security;

pub use agent::{AgentConfig, PhaseOverride};
pub use beads::BeadsConfig;
pub use claude::ClaudeConfig;
pub use error::LoomConfigError;
pub use exit_signals::ExitSignalsConfig;
pub use logs::LogsConfig;
pub use loop_config::LoopConfig;
pub use security::SecurityConfig;

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LoomConfig {
    pub pinned_context: String,
    pub beads: BeadsConfig,
    #[serde(rename = "loop")]
    pub loop_: LoopConfig,
    pub logs: LogsConfig,
    pub exit_signals: ExitSignalsConfig,
    pub agent: AgentConfig,
    pub claude: ClaudeConfig,
    pub security: SecurityConfig,
}

impl Default for LoomConfig {
    fn default() -> Self {
        Self {
            pinned_context: "docs/README.md".to_string(),
            beads: BeadsConfig::default(),
            loop_: LoopConfig::default(),
            logs: LogsConfig::default(),
            exit_signals: ExitSignalsConfig::default(),
            agent: AgentConfig::default(),
            claude: ClaudeConfig::default(),
            security: SecurityConfig::default(),
        }
    }
}

impl LoomConfig {
    /// Parse a `LoomConfig` from a TOML string. An empty string yields the
    /// full default config.
    pub fn from_toml_str(src: &str) -> Result<Self, LoomConfigError> {
        Ok(toml::from_str(src)?)
    }

    /// Load a config from disk. A missing file yields the default config so
    /// `.wrapix/loom/config.toml` is optional.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LoomConfigError> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml_str(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(LoomConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    /// The example TOML reproduced verbatim from the Configuration section of
    /// `specs/loom-harness.md`. Lines are unmodified so the test guards
    /// against drift between the spec and parser.
    const SPEC_EXAMPLE: &str = r#"pinned_context = "docs/README.md"

[beads]
priority = 2
default_type = "task"

[loop]
max_iterations = 3
max_retries = 2
max_reviews = 2

[logs]
# Delete log files under .wrapix/loom/logs/ older than this many days on
# `loom run` startup. 0 disables sweeping (keep forever).
retention_days = 14

[exit_signals]
complete = "RALPH_COMPLETE"
blocked = "RALPH_BLOCKED:"
clarify = "RALPH_CLARIFY:"

[agent]
default = "claude"

# Per-phase overrides: backend + model. Phases without overrides inherit default.
# [agent.todo]
# backend = "pi"
# provider = "deepseek"
# model_id = "deepseek-v3"
#
# [agent.check]
# backend = "claude"

[claude]
# Seconds to wait for clean exit after `result` before SIGTERM. Mirrors
# ralph's RALPH_CLAUDE_POST_RESULT_GRACE.
post_result_grace_secs = 5

[security]
# Tool names to deny when Claude sends control_request. Claude-backend only —
# pi has no host-side permission flow (tools execute internally, no
# control_request analog). Empty by default — the container sandbox is the
# trust boundary.
# denied_tools = ["SomeNewHostTool"]
"#;

    #[test]
    fn empty_string_yields_defaults() -> Result<()> {
        let cfg = LoomConfig::from_toml_str("")?;
        assert_eq!(cfg, LoomConfig::default());
        Ok(())
    }

    #[test]
    fn spec_example_matches_defaults() -> Result<()> {
        let cfg = LoomConfig::from_toml_str(SPEC_EXAMPLE)?;
        assert_eq!(cfg, LoomConfig::default());
        Ok(())
    }

    #[test]
    fn partial_file_fills_remaining_with_defaults() -> Result<()> {
        let src = r#"
pinned_context = "AGENTS.md"

[loop]
max_retries = 5
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        assert_eq!(cfg.pinned_context, "AGENTS.md");
        assert_eq!(cfg.loop_.max_retries, 5);
        // Other [loop] fields fall back to defaults.
        assert_eq!(cfg.loop_.max_iterations, 3);
        assert_eq!(cfg.loop_.max_reviews, 2);
        // Whole sections that are absent stay at defaults.
        assert_eq!(cfg.beads, BeadsConfig::default());
        assert_eq!(cfg.exit_signals, ExitSignalsConfig::default());
        assert_eq!(cfg.agent, AgentConfig::default());
        assert_eq!(cfg.claude, ClaudeConfig::default());
        assert_eq!(cfg.security, SecurityConfig::default());
        Ok(())
    }

    #[test]
    fn agent_overrides_collect_into_map() -> Result<()> {
        let src = r#"
[agent]
default = "pi"

[agent.todo]
backend = "pi"
provider = "deepseek"
model_id = "deepseek-v3"

[agent.check]
backend = "claude"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        assert_eq!(cfg.agent.default, "pi");
        assert_eq!(cfg.agent.overrides.len(), 2);
        let todo = &cfg.agent.overrides["todo"];
        assert_eq!(todo.backend.as_deref(), Some("pi"));
        assert_eq!(todo.provider.as_deref(), Some("deepseek"));
        assert_eq!(todo.model_id.as_deref(), Some("deepseek-v3"));
        let check = &cfg.agent.overrides["check"];
        assert_eq!(check.backend.as_deref(), Some("claude"));
        assert!(check.provider.is_none());
        Ok(())
    }

    #[test]
    fn security_denied_tools_parses_list() -> Result<()> {
        let src = r#"
[security]
denied_tools = ["WebFetch", "DangerousTool"]
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        assert_eq!(cfg.security.denied_tools, vec!["WebFetch", "DangerousTool"]);
        Ok(())
    }

    #[test]
    fn load_missing_file_yields_defaults() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let cfg = LoomConfig::load(dir.path().join("does-not-exist.toml"))?;
        assert_eq!(cfg, LoomConfig::default());
        Ok(())
    }

    #[test]
    fn load_reads_file_from_disk() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "pinned_context = \"AGENTS.md\"\n")?;
        let cfg = LoomConfig::load(&path)?;
        assert_eq!(cfg.pinned_context, "AGENTS.md");
        Ok(())
    }

    #[test]
    fn invalid_toml_returns_parse_error() {
        let result = LoomConfig::from_toml_str("not = = valid");
        assert!(matches!(result, Err(LoomConfigError::Parse(_))));
    }
}
