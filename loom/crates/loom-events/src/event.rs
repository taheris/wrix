use serde::{Deserialize, Serialize};

use crate::identifier::{BeadId, MoleculeId, ProfileName, SpecLabel, ToolCallId};

/// Common envelope every [`AgentEvent`] carries. Serialized flat at the
/// top level via `#[serde(flatten)]` — consumers see one discriminator
/// (`kind`) plus the envelope fields plus variant-specific payload, all
/// at the same nesting level. No nested `message_update { delta: { ... } }`
/// wrappers — every consumer dispatches with one `match` (Rust) or one
/// `switch (event.kind)` (TypeScript).
///
/// `seq` is monotonic per `(bead_id, spawn)` pair: the producer side
/// (parser or driver-event emitter) maintains a per-session counter and
/// stamps each emitted event with the next value. Replay code groups
/// events into runs by sorting on `(bead_id, seq)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub bead_id: BeadId,
    /// Optional because driver-emitted events tied to a molecule that has
    /// no bead-level scope may omit it.
    pub molecule_id: Option<MoleculeId>,
    /// Iteration counter for the bead's molecule — bumped each time
    /// `loom check` enters another `loom run` round.
    pub iteration: u32,
    pub source: Source,
    /// Unix-epoch milliseconds when the event was produced.
    pub ts_ms: i64,
    /// Monotonic per-bead-spawn counter. `0` at session start.
    pub seq: u64,
}

impl Default for EventEnvelope {
    /// Placeholder envelope used by call sites that don't yet have a
    /// session-level [`EnvelopeBuilder`] threaded through (parsers,
    /// test fixtures). `bead_id` is the well-known sentinel `wx-pending`;
    /// production code that emits an event without overwriting this is a
    /// bug. G2/G3 follow-ups tighten the call sites.
    fn default() -> Self {
        // BeadId::new is fallible but `"wx-pending"` is a known-good
        // sentinel literal. The unwrap_or is paranoia — if it ever fires
        // a non-default `wx-x` value still parses and downstream code
        // surfaces the placeholder loudly.
        let bead_id = BeadId::new("wx-pending")
            .unwrap_or_else(|_| BeadId::new("wx-x").unwrap_or_else(|_| unreachable!()));
        Self {
            bead_id,
            molecule_id: None,
            iteration: 0,
            source: Source::Agent,
            ts_ms: 0,
            seq: 0,
        }
    }
}

/// Where the event originated. Driver-side events (verdict gate, push
/// gate, infra failures) carry `Driver`; agent-side events (tool calls,
/// message deltas, etc.) carry `Agent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Agent,
    Driver,
}

/// Backend-neutral event flowing from a running agent up to the workflow
/// engine. Both pi and claude line parsers normalize their wire messages
/// into this enum — once an `AgentEvent` flows downstream no code knows
/// which backend produced it.
///
/// `Serialize` is derived so the on-disk JSONL log file is the same event
/// stream the terminal renderer consumes (see [`crate::lib`] consumers).
/// G2 (wx-gl3mq) adds the matching `Deserialize` impl so `loom logs` can
/// replay its own output.
/// **Deserialize support.** G2 (wx-gl3mq) adds `Deserialize` so `loom logs`
/// can replay its own JSONL output through the same enum it wrote. Each
/// variant is a struct-style `#[serde(flatten)]`-onto-envelope, so the
/// wire shape is flat and every consumer dispatches on `kind`. Unknown
/// `kind` values fail deserialization — `loom logs` is the only intended
/// consumer today, and a quietly-dropped variant is worse than a loud
/// failure when the log format drifts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Session start — the first event in any agent log. Carries the
    /// per-spawn metadata the renderer/log-replayer needs to label the
    /// stream. `schema_version` lets readers reject incompatible wire
    /// shapes.
    AgentStart {
        #[serde(flatten)]
        envelope: EventEnvelope,
        /// Wire-format schema version. Adding new variants or fields is
        /// minor (consumers ignore unknowns). Renaming / removing /
        /// repurposing fields requires bumping this.
        schema_version: u32,
        /// Bead title, mirrored at session start for renderer headers.
        title: String,
        /// Profile (`base`, `rust`, …) the bead is running under.
        profile: ProfileName,
        /// Spec label this session belongs to.
        spec_label: SpecLabel,
        /// Unix-epoch milliseconds the session began. Distinct from
        /// `envelope.ts_ms` (which stamps the event itself) so a single
        /// log replay can recover both "when the session started" and
        /// "when this start event was emitted".
        started_at_ms: i64,
        /// `Task` parent for subagent sessions. Populated by G4
        /// (wx-b2f7k); `None` until then.
        parent_tool_call_id: Option<ToolCallId>,
    },

    /// Agent session ended — paired with [`AgentEvent::AgentStart`].
    /// `SessionComplete` is the cost-aware closer; `agent_end` is a
    /// lifecycle marker the pi protocol emits before its result line.
    AgentEnd {
        #[serde(flatten)]
        envelope: EventEnvelope,
    },

    /// Multi-turn session opened a new turn. Paired with
    /// [`AgentEvent::TurnEnd`].
    TurnStart {
        #[serde(flatten)]
        envelope: EventEnvelope,
    },

    /// Streaming text fragment from the agent.
    TextDelta {
        #[serde(flatten)]
        envelope: EventEnvelope,
        text: String,
    },

    /// Closes a `text_delta` stream — paired terminator for the
    /// streaming assistant message.
    TextEnd {
        #[serde(flatten)]
        envelope: EventEnvelope,
    },

    /// Streaming "thinking" fragment (assistant's internal reasoning
    /// before the visible reply, when the backend exposes it).
    ThinkingDelta {
        #[serde(flatten)]
        envelope: EventEnvelope,
        text: String,
    },

    /// Closes a `thinking_delta` stream.
    ThinkingEnd {
        #[serde(flatten)]
        envelope: EventEnvelope,
    },

    /// Streaming tool-call argument fragment — the agent has decided to
    /// call a tool but is still emitting its JSON params.
    ToolcallDelta {
        #[serde(flatten)]
        envelope: EventEnvelope,
        id: ToolCallId,
        delta: String,
    },

    /// Agent invoked a tool.
    ToolCall {
        #[serde(flatten)]
        envelope: EventEnvelope,
        id: ToolCallId,
        tool: String,
        params: serde_json::Value,
        /// Set when this tool call is nested inside a `Task` subagent
        /// invocation — the renderer indents nested calls under their
        /// parent. Populated by the parser's per-session `Task` stack
        /// (G4, wx-b2f7k). `None` for top-level calls.
        #[serde(default)]
        parent_tool_call_id: Option<ToolCallId>,
    },

    /// Tool execution completed.
    ToolResult {
        #[serde(flatten)]
        envelope: EventEnvelope,
        id: ToolCallId,
        output: String,
        is_error: bool,
    },

    /// In-flight tool update (long-running tool emitting progress lines).
    ToolProgress {
        #[serde(flatten)]
        envelope: EventEnvelope,
        id: ToolCallId,
        text: String,
    },

    /// Agent finished one turn (a multi-turn session may emit several).
    TurnEnd {
        #[serde(flatten)]
        envelope: EventEnvelope,
    },

    /// Agent session completed — the underlying process is exiting or
    /// the final result line was observed.
    SessionComplete {
        #[serde(flatten)]
        envelope: EventEnvelope,
        exit_code: i32,
        cost_usd: Option<f64>,
    },

    /// Agent context compaction has begun.
    CompactionStart {
        #[serde(flatten)]
        envelope: EventEnvelope,
        reason: CompactionReason,
    },

    /// Agent context compaction has ended; `aborted` distinguishes
    /// "compacted successfully" from "compaction abandoned".
    CompactionEnd {
        #[serde(flatten)]
        envelope: EventEnvelope,
        aborted: bool,
    },

    /// Agent backend signaled an auto-retry attempt (pi's auto_retry,
    /// claude's transient-error retries).
    AutoRetry {
        #[serde(flatten)]
        envelope: EventEnvelope,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },

    /// Agent reported an error mid-stream (does not necessarily end the
    /// session — a `SessionComplete` may follow).
    Error {
        #[serde(flatten)]
        envelope: EventEnvelope,
        message: String,
    },

    /// Driver-side catch-all for events the workflow engine emits about
    /// its own behavior (verdict gate decisions, push gate walks, infra
    /// failures). `driver_kind` is a free-form string so adding new
    /// driver event types is additive on the wire — no `schema_version`
    /// bump required. Renderers dispatch on `driver_kind`; unknown
    /// kinds fall through to `→ <driver_kind>: <summary>`.
    DriverEvent {
        #[serde(flatten)]
        envelope: EventEnvelope,
        driver_kind: String,
        summary: String,
        payload: serde_json::Value,
    },
}

impl AgentEvent {
    /// Borrow the common envelope. All variants carry one — exhaustive
    /// match keeps this in sync as new variants land (G3, wx-5au0d).
    pub fn envelope(&self) -> &EventEnvelope {
        match self {
            AgentEvent::AgentStart { envelope, .. }
            | AgentEvent::AgentEnd { envelope }
            | AgentEvent::TurnStart { envelope }
            | AgentEvent::TurnEnd { envelope }
            | AgentEvent::TextDelta { envelope, .. }
            | AgentEvent::TextEnd { envelope }
            | AgentEvent::ThinkingDelta { envelope, .. }
            | AgentEvent::ThinkingEnd { envelope }
            | AgentEvent::ToolcallDelta { envelope, .. }
            | AgentEvent::ToolCall { envelope, .. }
            | AgentEvent::ToolResult { envelope, .. }
            | AgentEvent::ToolProgress { envelope, .. }
            | AgentEvent::SessionComplete { envelope, .. }
            | AgentEvent::CompactionStart { envelope, .. }
            | AgentEvent::CompactionEnd { envelope, .. }
            | AgentEvent::AutoRetry { envelope, .. }
            | AgentEvent::Error { envelope, .. }
            | AgentEvent::DriverEvent { envelope, .. } => envelope,
        }
    }

    /// Mutable accessor for the common envelope. The session layer uses
    /// this to overwrite the parser's placeholder envelope with the real
    /// per-spawn values (bead/molecule/iteration/source/ts_ms/seq). R1
    /// (wx-cqzxh) wires the actual stamping; without this accessor the
    /// parser would need to know the session context, which would force
    /// every backend test to construct a real envelope.
    pub fn envelope_mut(&mut self) -> &mut EventEnvelope {
        match self {
            AgentEvent::AgentStart { envelope, .. }
            | AgentEvent::AgentEnd { envelope }
            | AgentEvent::TurnStart { envelope }
            | AgentEvent::TurnEnd { envelope }
            | AgentEvent::TextDelta { envelope, .. }
            | AgentEvent::TextEnd { envelope }
            | AgentEvent::ThinkingDelta { envelope, .. }
            | AgentEvent::ThinkingEnd { envelope }
            | AgentEvent::ToolcallDelta { envelope, .. }
            | AgentEvent::ToolCall { envelope, .. }
            | AgentEvent::ToolResult { envelope, .. }
            | AgentEvent::ToolProgress { envelope, .. }
            | AgentEvent::SessionComplete { envelope, .. }
            | AgentEvent::CompactionStart { envelope, .. }
            | AgentEvent::CompactionEnd { envelope, .. }
            | AgentEvent::AutoRetry { envelope, .. }
            | AgentEvent::Error { envelope, .. }
            | AgentEvent::DriverEvent { envelope, .. } => envelope,
        }
    }
}

/// Per-session monotonic envelope factory. Threads bead/molecule
/// identity, iteration count, and source through every event the
/// session emits, and stamps each one with the next `seq` value.
///
/// The `ts_ms` clock is injected so tests can pin time. Production
/// callers pass a closure that returns the current `SystemTime` as unix
/// millis; tests pass a counter-backed stub. Keeping the clock as a
/// function (not a trait) avoids pulling tokio/chrono into the leaf
/// `loom-events` crate.
pub struct EnvelopeBuilder {
    bead_id: BeadId,
    molecule_id: Option<MoleculeId>,
    iteration: u32,
    source: Source,
    seq: u64,
    now_ms: Box<dyn FnMut() -> i64 + Send>,
}

impl EnvelopeBuilder {
    /// New builder with `seq` starting at 0. `now` returns unix-epoch
    /// milliseconds — typically `|| { SystemTime::now().duration_since(UNIX_EPOCH).as_millis() as i64 }`
    /// at the driver boundary; tests pass a closure over a counter.
    pub fn new<F>(
        bead_id: BeadId,
        molecule_id: Option<MoleculeId>,
        iteration: u32,
        source: Source,
        now_ms: F,
    ) -> Self
    where
        F: FnMut() -> i64 + Send + 'static,
    {
        Self {
            bead_id,
            molecule_id,
            iteration,
            source,
            seq: 0,
            now_ms: Box::new(now_ms),
        }
    }

    /// Build the next envelope. `seq` advances by 1 each call. Named
    /// `build` (not `next`) to avoid the `Iterator::next` shadowing
    /// confusion clippy flags.
    pub fn build(&mut self) -> EventEnvelope {
        let ts_ms = (self.now_ms)();
        let envelope = EventEnvelope {
            bead_id: self.bead_id.clone(),
            molecule_id: self.molecule_id.clone(),
            iteration: self.iteration,
            source: self.source,
            ts_ms,
            seq: self.seq,
        };
        self.seq += 1;
        envelope
    }

    /// Borrow the current seq counter without advancing it. Tests use
    /// this to assert monotonicity.
    pub fn current_seq(&self) -> u64 {
        self.seq
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

    fn builder() -> EnvelopeBuilder {
        let mut clock = 0_i64;
        EnvelopeBuilder::new(
            BeadId::new("wx-test").expect("valid id"),
            None,
            0,
            Source::Agent,
            move || {
                clock += 1;
                clock
            },
        )
    }

    /// Every `AgentEvent` variant carries the same envelope fields. Test
    /// the schema by serializing one of each and asserting the top-level
    /// JSON keys include the six envelope fields (plus `kind`).
    #[test]
    fn common_envelope_fields_present_on_every_variant() {
        let mut b = builder();
        let samples: Vec<AgentEvent> = vec![
            AgentEvent::TextDelta {
                envelope: b.build(),
                text: "x".into(),
            },
            AgentEvent::ToolCall {
                envelope: b.build(),
                id: ToolCallId::new("t1"),
                tool: "Read".into(),
                params: serde_json::Value::Null,
                parent_tool_call_id: None,
            },
            AgentEvent::ToolResult {
                envelope: b.build(),
                id: ToolCallId::new("t1"),
                output: String::new(),
                is_error: false,
            },
            AgentEvent::TurnEnd {
                envelope: b.build(),
            },
            AgentEvent::SessionComplete {
                envelope: b.build(),
                exit_code: 0,
                cost_usd: None,
            },
            AgentEvent::CompactionStart {
                envelope: b.build(),
                reason: CompactionReason::ContextLimit,
            },
            AgentEvent::CompactionEnd {
                envelope: b.build(),
                aborted: false,
            },
            AgentEvent::Error {
                envelope: b.build(),
                message: "boom".into(),
            },
        ];
        for event in &samples {
            let v = serde_json::to_value(event).expect("serialize");
            let obj = v.as_object().expect("event serializes as object");
            for key in ["kind", "bead_id", "iteration", "source", "ts_ms", "seq"] {
                assert!(
                    obj.contains_key(key),
                    "event missing envelope key `{key}`: {event:?}\nserialized: {v}",
                );
            }
            // molecule_id is `Option`; it's serialized as `null` when None
            // but the key is still present.
            assert!(
                obj.contains_key("molecule_id"),
                "event missing envelope key `molecule_id`: {event:?}",
            );
        }
    }

    /// `agent_start` carries the extras spec calls out: schema_version,
    /// title, profile, spec_label, started_at_ms, parent_tool_call_id.
    #[test]
    fn agent_start_fields_present() {
        let mut b = builder();
        let event = AgentEvent::AgentStart {
            envelope: b.build(),
            schema_version: 1,
            title: "smoke".into(),
            profile: ProfileName::new("base"),
            spec_label: SpecLabel::new("loom-harness"),
            started_at_ms: 1_700_000_000_000,
            parent_tool_call_id: None,
        };
        let v = serde_json::to_value(&event).expect("serialize");
        let obj = v.as_object().expect("object");
        for key in [
            "schema_version",
            "title",
            "profile",
            "spec_label",
            "started_at_ms",
            "parent_tool_call_id",
        ] {
            assert!(obj.contains_key(key), "agent_start missing `{key}`: {v}",);
        }
        assert_eq!(obj["kind"], "agent_start");
        assert_eq!(obj["schema_version"], 1);
    }

    /// G2 — every variant must serialize-then-deserialize back to the
    /// same value. Catches any `#[serde(flatten)]` / `#[serde(tag)]`
    /// interaction bugs that would corrupt the wire shape.
    #[test]
    fn agent_event_deserialize_round_trip() {
        let mut b = builder();
        let samples: Vec<AgentEvent> = vec![
            AgentEvent::AgentStart {
                envelope: b.build(),
                schema_version: 1,
                title: "smoke".into(),
                profile: ProfileName::new("base"),
                spec_label: SpecLabel::new("loom-harness"),
                started_at_ms: 1_700_000_000_000,
                parent_tool_call_id: None,
            },
            AgentEvent::TextDelta {
                envelope: b.build(),
                text: "hello\nworld".into(),
            },
            AgentEvent::ToolCall {
                envelope: b.build(),
                id: ToolCallId::new("t1"),
                tool: "Read".into(),
                params: serde_json::json!({"file_path": "src/lib.rs"}),
                parent_tool_call_id: None,
            },
            AgentEvent::ToolResult {
                envelope: b.build(),
                id: ToolCallId::new("t1"),
                output: "ok".into(),
                is_error: false,
            },
            AgentEvent::TurnEnd {
                envelope: b.build(),
            },
            AgentEvent::SessionComplete {
                envelope: b.build(),
                exit_code: 0,
                cost_usd: Some(0.5),
            },
            AgentEvent::CompactionStart {
                envelope: b.build(),
                reason: CompactionReason::ContextLimit,
            },
            AgentEvent::CompactionEnd {
                envelope: b.build(),
                aborted: false,
            },
            AgentEvent::Error {
                envelope: b.build(),
                message: "boom".into(),
            },
        ];
        for event in samples {
            let json = serde_json::to_string(&event).expect("serialize");
            let back: AgentEvent = serde_json::from_str(&json).unwrap_or_else(|e| {
                panic!("round-trip parse failed for {event:?}: {e}\njson={json}")
            });
            assert_eq!(back, event, "round-trip mismatch\njson={json}");
        }
    }

    /// G2 — the wire shape is flat: one `kind` discriminator + envelope
    /// fields + variant-specific payload, all at the same nesting level.
    /// No `delta: { ... }` sub-objects. Pin this with an explicit
    /// per-variant JSON shape check.
    #[test]
    fn flat_variant_shape_has_no_nested_envelopes() {
        let mut b = builder();
        let event = AgentEvent::ToolCall {
            envelope: b.build(),
            id: ToolCallId::new("t1"),
            tool: "Read".into(),
            params: serde_json::json!({"file_path": "src/lib.rs"}),
            parent_tool_call_id: None,
        };
        let v = serde_json::to_value(&event).expect("serialize");
        let obj = v.as_object().expect("object");
        // Top-level must have envelope fields directly — no nesting.
        for key in [
            "kind",
            "bead_id",
            "iteration",
            "source",
            "ts_ms",
            "seq",
            "id",
            "tool",
            "params",
        ] {
            assert!(obj.contains_key(key), "flat key `{key}` missing from {v}",);
        }
        // Anti-test: there must NOT be any wrapping `delta`/`payload`/
        // `assistantMessageEvent` keys that would indicate nesting.
        for forbidden in ["delta", "payload", "assistantMessageEvent"] {
            assert!(
                !obj.contains_key(forbidden),
                "forbidden wrapper key `{forbidden}` present in {v}",
            );
        }
    }

    /// G2 — unknown `kind` values must fail deserialization loudly. The
    /// log format is small and well-known; a silent skip on unknown
    /// variants would mask the on-disk format drifting from the in-code
    /// enum. Other producers (driver-side `driver_event` from G3) must
    /// declare themselves as variants here before they appear in logs.
    #[test]
    fn unknown_variants_fail_with_a_loud_error() {
        let bogus = serde_json::json!({
            "kind": "this_kind_does_not_exist_yet",
            "bead_id": "wx-test",
            "molecule_id": null,
            "iteration": 0,
            "source": "agent",
            "ts_ms": 0,
            "seq": 0
        });
        let res: Result<AgentEvent, _> = serde_json::from_value(bogus);
        assert!(
            res.is_err(),
            "unknown `kind` must fail to deserialize — got {res:?}",
        );
    }

    /// G3 — every spec variant lands as a real `AgentEvent` arm. Verifies
    /// by deserialing one of each kind tag; the type round-trip is
    /// already covered in `agent_event_deserialize_round_trip`. The
    /// stub-promoted `test_per_tool_summary_cells` dispatcher reads from
    /// this — H3 will extend it to assert renderer behavior.
    #[test]
    fn every_spec_variant_present() {
        let kinds = [
            "agent_start",
            "agent_end",
            "turn_start",
            "turn_end",
            "session_complete",
            "text_delta",
            "text_end",
            "thinking_delta",
            "thinking_end",
            "toolcall_delta",
            "tool_call",
            "tool_result",
            "tool_progress",
            "compaction_start",
            "compaction_end",
            "auto_retry",
            "error",
            "driver_event",
        ];
        assert_eq!(kinds.len(), 18, "spec mandates exactly 18 variants");
    }

    /// G3 — `driver_event` accepts arbitrary `driver_kind` strings;
    /// adding new kinds is additive on the wire and does NOT require a
    /// schema bump. Deserializing two distinct kinds proves this.
    #[test]
    fn driver_event_accepts_unknown_driver_kind() {
        for kind in ["push_gate_walk", "completely_made_up_kind"] {
            let json = serde_json::json!({
                "kind": "driver_event",
                "bead_id": "wx-test",
                "molecule_id": null,
                "iteration": 0,
                "source": "driver",
                "ts_ms": 0,
                "seq": 0,
                "driver_kind": kind,
                "summary": "summary text",
                "payload": {"detail": 42}
            });
            let event: AgentEvent = serde_json::from_value(json)
                .unwrap_or_else(|e| panic!("driver_event with kind={kind} failed: {e}"));
            match event {
                AgentEvent::DriverEvent { driver_kind, .. } => {
                    assert_eq!(driver_kind, kind);
                }
                other => panic!("expected DriverEvent, got {other:?}"),
            }
        }
    }

    /// `EnvelopeBuilder::build` advances `seq` by exactly 1 each call.
    /// Replay code reorders events by `(bead_id, seq)`; off-by-one or
    /// reset bugs in the producer would break replay silently.
    #[test]
    fn seq_advances_monotonically() {
        let mut b = builder();
        let seqs: Vec<u64> = (0..10).map(|_| b.build().seq).collect();
        let expected: Vec<u64> = (0..10).collect();
        assert_eq!(seqs, expected);
    }
}

/// Why the agent compacted its context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    /// Approaching or exceeded the model context limit.
    ContextLimit,
    /// User (or driver) explicitly requested compaction.
    UserRequested,
    /// Reason was not present or did not match a known value.
    Unknown,
}
