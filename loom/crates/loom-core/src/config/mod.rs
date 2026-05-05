//! Loom configuration loaded from `.wrapix/loom/config.toml`.
//!
//! Parsed natively via the `toml` crate into a typed [`LoomConfig`]. Every
//! field carries `#[serde(default)]` so a missing or empty file yields
//! defaults that match Ralph's `.wrapix/ralph/config.nix`, letting users
//! transition without writing a Loom config.
//!
//! Per-phase agent and profile selection lives in `[phase.<name>]` tables
//! with `[phase.default]` as the fallback (see
//! `specs/loom-harness.md` § Configuration). Resolution for any phase
//! field walks `[phase.<name>]` → `[phase.default]` → built-in defaults.

mod agent;
mod beads;
mod claude;
mod error;
mod logs;
mod loop_config;
mod security;

pub use agent::{
    AgentSelection, AgentSelectionError, BUILT_IN_BACKEND, BUILT_IN_PROFILE, ClaudeSettings,
    DEFAULT_PHASE_KEY, Phase, PhaseAgentConfig, PhaseConfig, parse_backend_name,
};
pub use beads::BeadsConfig;
pub use claude::ClaudeConfig;
pub use error::LoomConfigError;
pub use logs::LogsConfig;
pub use loop_config::LoopConfig;
pub use security::SecurityConfig;

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::agent::AgentKind;
use crate::identifier::ProfileName;
use agent::lookup_phase_field;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LoomConfig {
    pub pinned_context: String,
    pub beads: BeadsConfig,
    #[serde(rename = "loop")]
    pub loop_: LoopConfig,
    pub logs: LogsConfig,
    /// `[phase.<name>]` tables keyed by phase name. The literal key
    /// `default` is the fallback applied by [`LoomConfig::agent_for`] to
    /// any field a per-phase table does not declare.
    pub phase: BTreeMap<String, PhaseConfig>,
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
            phase: BTreeMap::new(),
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

    /// Resolve the [`AgentSelection`] for `phase`. Each field walks the
    /// `[phase.<name>]` → `[phase.default]` → built-in chain; when the
    /// resolved backend is [`crate::agent::AgentKind::Claude`] the
    /// claude-specific settings are pulled from `[claude]` and `[security]`
    /// so call sites receive everything in one struct.
    ///
    /// Returns [`AgentSelectionError::UnknownBackend`] when the backend name
    /// (per-phase or default) does not match `claude` or `pi` — surfacing
    /// the validation lazily lets the TOML parser stay schema-free for
    /// unknown `[phase.<phase>]` keys.
    pub fn agent_for(&self, phase: Phase) -> Result<AgentSelection, AgentSelectionError> {
        let key = phase.as_str();
        let profile_str = lookup_phase_field(&self.phase, key, |p| &p.profile)
            .map(String::as_str)
            .unwrap_or(BUILT_IN_PROFILE);
        let backend_str = lookup_phase_field(&self.phase, key, |p| &p.agent.backend)
            .map(String::as_str)
            .unwrap_or(BUILT_IN_BACKEND);
        let kind = parse_backend_name(backend_str)?;
        let provider = lookup_phase_field(&self.phase, key, |p| &p.agent.provider).cloned();
        let model_id = lookup_phase_field(&self.phase, key, |p| &p.agent.model_id).cloned();
        let claude_settings = match kind {
            AgentKind::Claude => Some(ClaudeSettings {
                denied_tools: self.security.denied_tools.clone(),
                post_result_grace_secs: self.claude.post_result_grace_secs,
            }),
            AgentKind::Pi => None,
        };
        Ok(AgentSelection {
            profile: ProfileName::new(profile_str),
            kind,
            provider,
            model_id,
            claude_settings,
        })
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
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;
    use anyhow::Result;

    /// The example TOML reproduced verbatim from the Configuration section
    /// of `specs/loom-harness.md`. Any drift between the parser and the
    /// spec example surfaces here. Note the example explicitly writes the
    /// built-in defaults under `[phase.default]`; we do not assert
    /// `cfg == LoomConfig::default()` because the populated map and
    /// `BTreeMap::new()` are not structurally equal — instead the test
    /// asserts that `agent_for` resolves the same values either way.
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

# Per-phase config. Resolution for any field: [phase.<name>] →
# [phase.default] → built-in. `loom run` reads its profile from the
# bead's `profile:X` label first, then [phase.run] / [phase.default];
# the `--profile` CLI flag overrides everything.
[phase.default]
profile = "base"
agent.backend = "claude"

# [phase.todo]
# profile = "rust"
# agent.backend = "pi"
# agent.provider = "deepseek"
# agent.model_id = "deepseek-v3"
#
# [phase.check]
# agent.backend = "claude"

[claude]
# Agent-runtime settings, applied wherever claude is selected. Seconds to
# wait for clean exit after `result` before SIGTERM (shutdown watchdog).
post_result_grace_secs = 5

[security]
# Tool names to deny when claude sends control_request. Claude-only —
# pi has no host-side permission flow (tools execute internally, no
# control_request analog). Empty by default; the container sandbox is
# the trust boundary.
# denied_tools = ["SomeNewHostTool"]
"#;

    #[test]
    fn empty_string_yields_defaults() -> Result<()> {
        let cfg = LoomConfig::from_toml_str("")?;
        assert_eq!(cfg, LoomConfig::default());
        Ok(())
    }

    /// The spec example writes the built-in defaults explicitly under
    /// `[phase.default]`; both that form and the empty config resolve to
    /// the same values via `agent_for` for every phase.
    #[test]
    fn spec_example_resolves_to_built_in_defaults() -> Result<()> {
        let from_spec = LoomConfig::from_toml_str(SPEC_EXAMPLE)?;
        let empty = LoomConfig::default();
        for phase in [
            Phase::Plan,
            Phase::Todo,
            Phase::Run,
            Phase::Check,
            Phase::Msg,
        ] {
            let from_spec_sel = from_spec.agent_for(phase).expect("agent_for");
            let empty_sel = empty.agent_for(phase).expect("agent_for");
            assert_eq!(from_spec_sel, empty_sel, "phase={phase:?}");
            assert_eq!(from_spec_sel.profile.as_str(), BUILT_IN_PROFILE);
            assert_eq!(from_spec_sel.kind, AgentKind::Claude);
        }
        // Non-phase fields round-trip identically.
        assert_eq!(from_spec.pinned_context, empty.pinned_context);
        assert_eq!(from_spec.beads, empty.beads);
        assert_eq!(from_spec.loop_, empty.loop_);
        assert_eq!(from_spec.logs, empty.logs);
        assert_eq!(from_spec.claude, empty.claude);
        assert_eq!(from_spec.security, empty.security);
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
        assert!(cfg.phase.is_empty());
        assert_eq!(cfg.claude, ClaudeConfig::default());
        assert_eq!(cfg.security, SecurityConfig::default());
        Ok(())
    }

    #[test]
    fn phase_tables_collect_into_map() -> Result<()> {
        let src = r#"
[phase.default]
profile = "base"
agent.backend = "pi"

[phase.todo]
profile = "rust"
agent.backend = "pi"
agent.provider = "deepseek"
agent.model_id = "deepseek-v3"

[phase.check]
agent.backend = "claude"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        assert_eq!(cfg.phase.len(), 3);

        let default = &cfg.phase[DEFAULT_PHASE_KEY];
        assert_eq!(default.profile.as_deref(), Some("base"));
        assert_eq!(default.agent.backend.as_deref(), Some("pi"));

        let todo = &cfg.phase["todo"];
        assert_eq!(todo.profile.as_deref(), Some("rust"));
        assert_eq!(todo.agent.backend.as_deref(), Some("pi"));
        assert_eq!(todo.agent.provider.as_deref(), Some("deepseek"));
        assert_eq!(todo.agent.model_id.as_deref(), Some("deepseek-v3"));

        let check = &cfg.phase["check"];
        assert!(check.profile.is_none());
        assert_eq!(check.agent.backend.as_deref(), Some("claude"));
        assert!(check.agent.provider.is_none());
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

    /// `[phase.default] agent.backend = "claude"` with `[phase.todo]
    /// agent.backend = "pi"` → `agent_for(Todo)` returns `Pi`,
    /// `agent_for(Run)` inherits `Claude` from default.
    #[test]
    fn agent_for_per_phase_resolves_override_and_default() -> Result<()> {
        let src = r#"
[phase.default]
profile = "base"
agent.backend = "claude"

[phase.todo]
profile = "rust"
agent.backend = "pi"
agent.provider = "deepseek"
agent.model_id = "deepseek-v3"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;

        let todo = cfg.agent_for(Phase::Todo).expect("agent_for todo");
        assert_eq!(todo.profile.as_str(), "rust");
        assert_eq!(todo.kind, AgentKind::Pi);
        assert_eq!(todo.provider.as_deref(), Some("deepseek"));
        assert_eq!(todo.model_id.as_deref(), Some("deepseek-v3"));
        assert!(todo.claude_settings.is_none());

        let run = cfg.agent_for(Phase::Run).expect("agent_for run");
        assert_eq!(run.profile.as_str(), "base");
        assert_eq!(run.kind, AgentKind::Claude);
        assert!(run.provider.is_none());
        let claude = run.claude_settings.expect("claude_settings");
        assert_eq!(claude.post_result_grace_secs, 5);
        assert!(claude.denied_tools.is_empty());

        Ok(())
    }

    /// Empty config (no `[phase]` tables at all) resolves every phase to
    /// `claude` with the built-in `base` profile — the documented defaults.
    #[test]
    fn agent_for_default_is_claude_when_config_empty() -> Result<()> {
        let cfg = LoomConfig::default();
        for phase in [
            Phase::Plan,
            Phase::Todo,
            Phase::Run,
            Phase::Check,
            Phase::Msg,
        ] {
            let sel = cfg.agent_for(phase).expect("agent_for");
            assert_eq!(sel.kind, AgentKind::Claude, "phase={phase:?}");
            assert_eq!(sel.profile.as_str(), BUILT_IN_PROFILE, "phase={phase:?}");
            assert!(sel.claude_settings.is_some());
        }
        Ok(())
    }

    /// `[phase.default]` without `agent.backend` still resolves to the
    /// built-in `claude` backend; the per-field fallback chain reaches
    /// past the partially-populated default into the built-in.
    #[test]
    fn agent_for_falls_through_partial_default_to_built_in() -> Result<()> {
        let src = r#"
[phase.default]
profile = "base"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        let sel = cfg.agent_for(Phase::Run).expect("agent_for");
        assert_eq!(sel.kind, AgentKind::Claude);
        assert_eq!(sel.profile.as_str(), "base");
        Ok(())
    }

    /// Unknown backend name in TOML surfaces as `UnknownBackend` — not a
    /// parse error — so the message is precise about the offending value.
    #[test]
    fn agent_for_unknown_backend_in_default_returns_error() -> Result<()> {
        let src = r#"
[phase.default]
agent.backend = "gpt"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        match cfg.agent_for(Phase::Run) {
            Err(AgentSelectionError::UnknownBackend { name }) => assert_eq!(name, "gpt"),
            other => panic!("expected UnknownBackend, got {other:?}"),
        }
        Ok(())
    }

    /// Unknown backend in a per-phase override surfaces only when that phase
    /// is queried — other phases still resolve.
    #[test]
    fn agent_for_unknown_backend_in_phase_override_isolated_to_that_phase() -> Result<()> {
        let src = r#"
[phase.default]
agent.backend = "claude"

[phase.todo]
agent.backend = "ollama"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        match cfg.agent_for(Phase::Todo) {
            Err(AgentSelectionError::UnknownBackend { name }) => assert_eq!(name, "ollama"),
            other => panic!("expected UnknownBackend, got {other:?}"),
        }
        // Other phases unaffected.
        let run = cfg.agent_for(Phase::Run).expect("run unaffected");
        assert_eq!(run.kind, AgentKind::Claude);
        Ok(())
    }

    /// Claude-specific settings (`[claude]` + `[security]`) flow through
    /// `agent_for` when the resolved backend is claude.
    #[test]
    fn agent_for_threads_claude_specific_settings_when_kind_is_claude() -> Result<()> {
        let src = r#"
[claude]
post_result_grace_secs = 12

[security]
denied_tools = ["WebFetch", "Other"]
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        let sel = cfg.agent_for(Phase::Run).expect("agent_for");
        let claude = sel.claude_settings.expect("claude_settings present");
        assert_eq!(claude.post_result_grace_secs, 12);
        assert_eq!(claude.denied_tools, vec!["WebFetch", "Other"]);
        Ok(())
    }

    /// A per-phase `profile` override wins over `[phase.default].profile`.
    #[test]
    fn agent_for_resolves_profile_per_phase() -> Result<()> {
        let src = r#"
[phase.default]
profile = "base"

[phase.todo]
profile = "rust"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        assert_eq!(
            cfg.agent_for(Phase::Todo).expect("todo").profile.as_str(),
            "rust"
        );
        assert_eq!(
            cfg.agent_for(Phase::Run).expect("run").profile.as_str(),
            "base"
        );
        Ok(())
    }
}
