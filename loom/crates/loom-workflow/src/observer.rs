//! Default-observer composition for Loom's Pi/Claude session driver.
//!
//! `loom-llm` ships `DoomLoopObserver` + `DuplicateResultObserver` so
//! external consumers driving `Conversation::run` get the safety nets
//! out of the box; this module wires the same observers into the
//! binary's `run_agent_classified` event loop, sourced from
//! `LoomConfig`'s `[agent.doom_loop]` / `[agent.duplicate_result]`
//! blocks (`specs/loom-llm.md` § Agent-Loop Observers).
//!
//! The chain is a thin `EventSink` that fans `emit` / `react` into each
//! enabled observer in registration order — doom-loop first, then
//! duplicate-result — matching the `react()`-priority rule the workflow
//! enforces (`SessionCommand::Abort` short-circuits the batch).

use loom_driver::config::{
    AgentObserversConfig, DoomLoopConfig as DriverDoomLoopConfig,
    DuplicateResultConfig as DriverDuplicateResultConfig,
};
use loom_events::{AgentEvent, EventSink, SessionCommand};
use loom_llm::observer::{
    DoomLoopConfig as LlmDoomLoopConfig, DoomLoopObserver,
    DuplicateResultConfig as LlmDuplicateResultConfig, DuplicateResultObserver,
};

/// Observer chain composed in `run_agent_classified`. Wraps each
/// optional `loom-llm` observer and forwards `EventSink` calls in
/// registration order so the driver's `react()` priority rule applies
/// to the combined batch.
pub struct DefaultObserverChain {
    doom_loop: Option<DoomLoopObserver>,
    duplicate_result: Option<DuplicateResultObserver>,
}

impl DefaultObserverChain {
    /// Build a chain from a `LoomConfig::agent` block. Returns `None`
    /// when every sub-observer is disabled (`enabled = false`) so the
    /// caller can pass `None` to `run_agent_classified` without
    /// allocating an empty chain.
    pub fn from_config(config: &AgentObserversConfig) -> Option<Self> {
        let doom_loop = build_doom_loop(&config.doom_loop);
        let duplicate_result = build_duplicate_result(&config.duplicate_result);
        if doom_loop.is_none() && duplicate_result.is_none() {
            None
        } else {
            Some(Self {
                doom_loop,
                duplicate_result,
            })
        }
    }

    /// Whether the doom-loop observer is composed into this chain.
    pub fn doom_loop_enabled(&self) -> bool {
        self.doom_loop.is_some()
    }

    /// Whether the duplicate-result observer is composed into this
    /// chain.
    pub fn duplicate_result_enabled(&self) -> bool {
        self.duplicate_result.is_some()
    }
}

impl EventSink for DefaultObserverChain {
    fn emit(&mut self, event: &AgentEvent) {
        if let Some(observer) = self.doom_loop.as_mut() {
            observer.emit(event);
        }
        if let Some(observer) = self.duplicate_result.as_mut() {
            observer.emit(event);
        }
    }

    fn react(&mut self) -> Vec<SessionCommand> {
        let mut commands = Vec::new();
        if let Some(observer) = self.doom_loop.as_mut() {
            commands.extend(observer.react());
        }
        if let Some(observer) = self.duplicate_result.as_mut() {
            commands.extend(observer.react());
        }
        commands
    }
}

fn build_doom_loop(config: &DriverDoomLoopConfig) -> Option<DoomLoopObserver> {
    if !config.enabled {
        return None;
    }
    Some(DoomLoopObserver::from_config(&LlmDoomLoopConfig {
        enabled: config.enabled,
        window: config.window,
        threshold: config.threshold,
        stage_2_after_stage_1: config.stage_2_after_stage_1,
    }))
}

fn build_duplicate_result(config: &DriverDuplicateResultConfig) -> Option<DuplicateResultObserver> {
    if !config.enabled {
        return None;
    }
    Some(DuplicateResultObserver::from_config(
        &LlmDuplicateResultConfig {
            enabled: config.enabled,
            min_bytes: config.min_bytes,
        },
    ))
}

#[cfg(test)]
#[expect(clippy::panic, reason = "tests use panicking helpers")]
mod tests {
    use super::*;

    use loom_events::event::{EventEnvelope, Source};
    use loom_events::identifier::{BeadId, ToolCallId};
    use serde_json::json;

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            bead_id: BeadId::new("wx-test").expect("valid bead id"),
            molecule_id: None,
            iteration: 0,
            source: Source::Agent,
            ts_ms: seq as i64,
            seq,
        }
    }

    fn tool_call(seq: u64, id: &str, tool: &str, params: serde_json::Value) -> AgentEvent {
        AgentEvent::ToolCall {
            envelope: envelope(seq),
            id: ToolCallId::new(id),
            tool: tool.to_owned(),
            params,
            parent_tool_call_id: None,
        }
    }

    fn tool_result(seq: u64, id: &str, output: &str) -> AgentEvent {
        AgentEvent::ToolResult {
            envelope: envelope(seq),
            id: ToolCallId::new(id),
            output: output.to_owned(),
            is_error: false,
        }
    }

    /// Defaults compose both observers — the spec's safety nets are on
    /// for every Pi/Claude run unless the user opts out in TOML.
    #[test]
    fn default_config_composes_both_observers() {
        let chain = DefaultObserverChain::from_config(&AgentObserversConfig::default())
            .expect("default chain is non-empty");
        assert!(chain.doom_loop_enabled());
        assert!(chain.duplicate_result_enabled());
    }

    /// `[agent.doom_loop] enabled = false` drops the doom-loop observer
    /// from the chain but keeps the duplicate-result observer.
    #[test]
    fn disabled_doom_loop_drops_only_that_observer() {
        let cfg = AgentObserversConfig {
            doom_loop: DriverDoomLoopConfig {
                enabled: false,
                ..DriverDoomLoopConfig::default()
            },
            duplicate_result: DriverDuplicateResultConfig::default(),
        };
        let chain = DefaultObserverChain::from_config(&cfg).expect("non-empty");
        assert!(!chain.doom_loop_enabled());
        assert!(chain.duplicate_result_enabled());
    }

    /// Both `enabled = false` → `from_config` returns `None`, so the
    /// driver passes `None` to `run_agent_classified` and skips the
    /// observer arm entirely.
    #[test]
    fn both_disabled_returns_none() {
        let cfg = AgentObserversConfig {
            doom_loop: DriverDoomLoopConfig {
                enabled: false,
                ..DriverDoomLoopConfig::default()
            },
            duplicate_result: DriverDuplicateResultConfig {
                enabled: false,
                ..DriverDuplicateResultConfig::default()
            },
        };
        assert!(DefaultObserverChain::from_config(&cfg).is_none());
    }

    /// Stage 2 of the doom-loop observer surfaces through the chain's
    /// `react()` as `SessionCommand::Abort` — proving the driver's
    /// short-circuit rule will fire on a real doom-loop scenario.
    #[test]
    fn chain_react_propagates_doom_loop_abort() {
        let cfg = AgentObserversConfig {
            doom_loop: DriverDoomLoopConfig {
                enabled: true,
                window: 5,
                threshold: 3,
                stage_2_after_stage_1: 1,
            },
            duplicate_result: DriverDuplicateResultConfig::default(),
        };
        let mut chain = DefaultObserverChain::from_config(&cfg).expect("non-empty");
        let params = json!({"path": "/etc/hosts"});
        let same = r#"{"content":"loop"}"#;
        let mut seq = 0u64;
        for id in ["c1", "c2", "c3", "c4"] {
            chain.emit(&tool_call(seq, id, "read_file", params.clone()));
            seq += 1;
            chain.emit(&tool_result(seq, id, same));
            seq += 1;
        }
        let commands = chain.react();
        assert!(
            commands.iter().any(
                |c| matches!(c, SessionCommand::Abort(reason) if reason == "doom-loop: read_file")
            ),
            "stage 2 Abort must reach the driver via the chain: {commands:?}",
        );
    }

    /// Registration order = react order: doom-loop fires before
    /// duplicate-result. Verifies the chain's batch ordering matches
    /// the workflow's `classify_react_commands` priority rule (which
    /// reads commands in batch order).
    #[test]
    fn chain_react_orders_doom_loop_before_duplicate_result() {
        let cfg = AgentObserversConfig {
            doom_loop: DriverDoomLoopConfig {
                enabled: true,
                window: 5,
                threshold: 3,
                stage_2_after_stage_1: 10,
            },
            duplicate_result: DriverDuplicateResultConfig::default(),
        };
        let mut chain = DefaultObserverChain::from_config(&cfg).expect("non-empty");
        let params = json!({"path": "/etc/hosts"});
        let filler = "x".repeat(512);
        let same = format!(r#"{{"content":"loop","filler":"{filler}"}}"#);
        let mut seq = 0u64;
        for id in ["c1", "c2", "c3"] {
            chain.emit(&tool_call(seq, id, "read_file", params.clone()));
            seq += 1;
            chain.emit(&tool_result(seq, id, &same));
            seq += 1;
        }
        let commands = chain.react();
        assert!(matches!(
            commands.first(),
            Some(SessionCommand::Steer(msg)) if msg.contains("doom-loop suspected")
        ));
    }
}
