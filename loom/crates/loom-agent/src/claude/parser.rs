//! Claude Code stream-json line parser.
//!
//! [`LineParse`] impl that turns NDJSON lines from `claude --output-format
//! stream-json` into [`AgentEvent`]s and encodes driver-side stream-json
//! user messages (initial prompt, mid-session steering). Auto-approval of
//! `control_request` lines is keyed off a `denied_tools` set passed in by
//! the workflow layer (config-driven).

use std::collections::HashSet;

use loom_core::agent::{AgentEvent, LineParse, ParsedLine, ProtocolError};
use loom_core::identifier::RequestId;
use serde::Serialize;
use tracing::{info, trace};

use super::messages::{AssistantBlock, ClaudeMessage, UserBlock};

/// Claude Code stream-json line parser.
///
/// Stateless dispatch layer between
/// [`AgentSession`](loom_core::agent::AgentSession) and
/// [`messages::ClaudeMessage`](super::messages::ClaudeMessage). Auto-approves
/// every tool-permission `control_request` whose tool name is **not** in
/// `denied_tools`; denied tools receive `approved: false`.
pub struct ClaudeParser {
    denied_tools: HashSet<String>,
}

impl ClaudeParser {
    /// Build a parser with the configured deny-list. The list is loaded by
    /// the workflow layer from `[agent.claude].denied_tools`; the parser
    /// owns no policy of its own.
    pub fn new(denied_tools: Vec<String>) -> Self {
        Self {
            denied_tools: denied_tools.into_iter().collect(),
        }
    }
}

#[derive(Serialize)]
struct ControlResponse<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a RequestId,
    approved: bool,
}

#[derive(Serialize)]
struct UserMessageWire<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    message: UserMessageBody<'a>,
}

#[derive(Serialize)]
struct UserMessageBody<'a> {
    role: &'static str,
    content: &'a str,
}

impl LineParse for ClaudeParser {
    fn parse_line(&self, line: &str) -> Result<ParsedLine, ProtocolError> {
        let msg: ClaudeMessage = serde_json::from_str(line)?;
        match msg {
            ClaudeMessage::System {
                subtype,
                session_id,
            } => {
                if subtype == "init"
                    && let Some(sid) = session_id
                {
                    info!(session_id = %sid, "claude session initialized");
                }
                Ok(ParsedLine {
                    events: Vec::new(),
                    response: None,
                })
            }
            ClaudeMessage::Assistant { message } => {
                let mut events = Vec::with_capacity(message.content.len());
                for block in message.content {
                    match block {
                        AssistantBlock::Text { text } => {
                            events.push(AgentEvent::MessageDelta { text });
                        }
                        AssistantBlock::ToolUse { id, name, input } => {
                            events.push(AgentEvent::ToolCall {
                                id,
                                tool: name,
                                params: input,
                            });
                        }
                        AssistantBlock::Unknown => {
                            trace!("unknown assistant content block");
                        }
                    }
                }
                Ok(ParsedLine {
                    events,
                    response: None,
                })
            }
            ClaudeMessage::User { message } => {
                let mut events = Vec::with_capacity(message.content.len());
                for block in message.content {
                    match block {
                        UserBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            let output = match content {
                                serde_json::Value::String(s) => s,
                                serde_json::Value::Null => String::new(),
                                other => other.to_string(),
                            };
                            events.push(AgentEvent::ToolResult {
                                id: tool_use_id,
                                output,
                                is_error,
                            });
                        }
                        UserBlock::Unknown => {
                            trace!("unknown user content block");
                        }
                    }
                }
                Ok(ParsedLine {
                    events,
                    response: None,
                })
            }
            ClaudeMessage::Result {
                subtype,
                result,
                total_cost_usd,
                ..
            } => {
                let events = match subtype.as_str() {
                    "success" => vec![
                        AgentEvent::TurnEnd,
                        AgentEvent::SessionComplete {
                            exit_code: 0,
                            cost_usd: total_cost_usd,
                        },
                    ],
                    "error" => vec![
                        AgentEvent::Error {
                            message: result.unwrap_or_default(),
                        },
                        AgentEvent::SessionComplete {
                            exit_code: 1,
                            cost_usd: total_cost_usd,
                        },
                    ],
                    other => {
                        trace!(subtype = other, "unknown result subtype");
                        Vec::new()
                    }
                };
                Ok(ParsedLine {
                    events,
                    response: None,
                })
            }
            ClaudeMessage::ControlRequest { id, tool, input } => {
                let serialized = input.to_string();
                let truncated: String = serialized.chars().take(200).collect();
                let approved = !self.denied_tools.contains(&tool);
                info!(
                    tool = %tool,
                    input = %truncated,
                    approved,
                    "claude tool permission request",
                );
                let resp = ControlResponse {
                    kind: "control_response",
                    id: &id,
                    approved,
                };
                let mut response = serde_json::to_string(&resp)?;
                response.push('\n');
                Ok(ParsedLine {
                    events: Vec::new(),
                    response: Some(response),
                })
            }
            ClaudeMessage::Unknown => {
                trace!("unknown claude message type");
                Ok(ParsedLine {
                    events: Vec::new(),
                    response: None,
                })
            }
        }
    }

    fn encode_prompt(&self, msg: &str) -> Result<String, ProtocolError> {
        encode_user_message(msg)
    }

    fn encode_steer(&self, msg: &str) -> Result<String, ProtocolError> {
        encode_user_message(msg)
    }

    fn encode_abort(&self) -> Result<Option<String>, ProtocolError> {
        // claude has no abort wire command — shutdown is via SIGTERM/SIGKILL
        // driven by the backend's watchdog (see wx-pkht8.8).
        Ok(None)
    }
}

fn encode_user_message(msg: &str) -> Result<String, ProtocolError> {
    let payload = UserMessageWire {
        kind: "user",
        message: UserMessageBody {
            role: "user",
            content: msg,
        },
    };
    let mut line = serde_json::to_string(&payload)?;
    line.push('\n');
    Ok(line)
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;
    use loom_core::agent::AgentEvent;

    fn parse(parser: &ClaudeParser, line: &str) -> ParsedLine {
        parser
            .parse_line(line)
            .expect("fixture line should parse cleanly")
    }

    fn empty() -> ClaudeParser {
        ClaudeParser::new(Vec::new())
    }

    // -- test_claude_stream_json_parsing -----------------------------------

    #[test]
    fn parses_system_init() {
        let line = r#"{"type":"system","subtype":"init","session_id":"sess-abc"}"#;
        let p = parse(&empty(), line);
        assert!(p.events.is_empty());
        assert!(p.response.is_none());
    }

    #[test]
    fn parses_assistant_text_and_tool_use() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"toolu_01","name":"Read","input":{"path":"/tmp/x"}}]}}"#;
        let p = parse(&empty(), line);
        assert_eq!(p.events.len(), 2);
        match &p.events[0] {
            AgentEvent::MessageDelta { text } => assert_eq!(text, "hi"),
            other => panic!("expected MessageDelta, got {other:?}"),
        }
        match &p.events[1] {
            AgentEvent::ToolCall { id, tool, params } => {
                assert_eq!(id.as_str(), "toolu_01");
                assert_eq!(tool, "Read");
                assert_eq!(params["path"], "/tmp/x");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parses_user_tool_result_string_content() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"file contents","is_error":false}]}}"#;
        let p = parse(&empty(), line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            AgentEvent::ToolResult {
                id,
                output,
                is_error,
            } => {
                assert_eq!(id.as_str(), "toolu_01");
                assert_eq!(output, "file contents");
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn parses_result_success() {
        let line = r#"{"type":"result","subtype":"success","total_cost_usd":0.42,"duration_ms":1234,"num_turns":3,"is_error":false}"#;
        let p = parse(&empty(), line);
        assert_eq!(p.events.len(), 2);
    }

    // -- test_claude_event_mapping -----------------------------------------

    #[test]
    fn result_success_yields_turn_end_then_session_complete() {
        let line = r#"{"type":"result","subtype":"success","total_cost_usd":0.10}"#;
        let p = parse(&empty(), line);
        assert!(matches!(p.events[0], AgentEvent::TurnEnd));
        match &p.events[1] {
            AgentEvent::SessionComplete {
                exit_code,
                cost_usd,
            } => {
                assert_eq!(*exit_code, 0);
                assert_eq!(*cost_usd, Some(0.10));
            }
            other => panic!("expected SessionComplete, got {other:?}"),
        }
    }

    #[test]
    fn result_error_yields_error_then_session_complete() {
        let line = r#"{"type":"result","subtype":"error","result":"boom","total_cost_usd":0.05}"#;
        let p = parse(&empty(), line);
        assert_eq!(p.events.len(), 2);
        match &p.events[0] {
            AgentEvent::Error { message } => assert_eq!(message, "boom"),
            other => panic!("expected Error, got {other:?}"),
        }
        match &p.events[1] {
            AgentEvent::SessionComplete {
                exit_code,
                cost_usd,
            } => {
                assert_eq!(*exit_code, 1);
                assert_eq!(*cost_usd, Some(0.05));
            }
            other => panic!("expected SessionComplete, got {other:?}"),
        }
    }

    // -- test_claude_cost_capture ------------------------------------------

    #[test]
    fn result_event_captures_cost_usd() {
        let line = r#"{"type":"result","subtype":"success","total_cost_usd":0.42}"#;
        let p = parse(&empty(), line);
        let last = p.events.last().expect("session complete present");
        match last {
            AgentEvent::SessionComplete { cost_usd, .. } => {
                assert_eq!(*cost_usd, Some(0.42));
            }
            other => panic!("expected SessionComplete, got {other:?}"),
        }
    }

    #[test]
    fn result_event_without_cost_yields_none() {
        let line = r#"{"type":"result","subtype":"success"}"#;
        let p = parse(&empty(), line);
        match p.events.last() {
            Some(AgentEvent::SessionComplete { cost_usd, .. }) => {
                assert!(cost_usd.is_none());
            }
            other => panic!("expected SessionComplete, got {other:?}"),
        }
    }

    // -- test_claude_unknown_events ----------------------------------------

    #[test]
    fn unknown_message_type_returns_empty_events() {
        let line = r#"{"type":"newfangled","extra":42}"#;
        let p = parse(&empty(), line);
        assert!(p.events.is_empty());
        assert!(p.response.is_none());
    }

    // -- test_claude_permission_autoapprove --------------------------------

    #[test]
    fn control_request_autoapproves_when_denylist_empty() {
        let line =
            r#"{"type":"control_request","id":"req_01","tool":"Read","input":{"path":"/tmp/x"}}"#;
        let p = parse(&empty(), line);
        assert!(p.events.is_empty());
        let resp = p.response.expect("control_response present");
        assert!(resp.contains(r#""type":"control_response""#));
        assert!(resp.contains(r#""id":"req_01""#));
        assert!(resp.contains(r#""approved":true"#));
        assert!(resp.ends_with('\n'));
    }

    #[test]
    fn control_request_denied_when_tool_in_denylist() {
        let parser = ClaudeParser::new(vec!["WebFetch".to_string()]);
        let line = r#"{"type":"control_request","id":"req_02","tool":"WebFetch","input":{"url":"https://example.com"}}"#;
        let p = parse(&parser, line);
        let resp = p.response.expect("control_response present");
        assert!(resp.contains(r#""approved":false"#));
        assert!(resp.contains(r#""id":"req_02""#));
    }

    #[test]
    fn control_request_denylist_does_not_affect_other_tools() {
        let parser = ClaudeParser::new(vec!["WebFetch".to_string()]);
        let line = r#"{"type":"control_request","id":"req_03","tool":"Read","input":{}}"#;
        let p = parse(&parser, line);
        let resp = p.response.expect("control_response present");
        assert!(resp.contains(r#""approved":true"#));
    }

    // -- encoder shape -----------------------------------------------------

    #[test]
    fn encode_prompt_emits_stream_json_user_message() {
        let parser = empty();
        let line = parser
            .encode_prompt("hello")
            .expect("encoder should succeed");
        assert!(line.ends_with('\n'));
        let trimmed = line.trim_end();
        let v: serde_json::Value =
            serde_json::from_str(trimmed).expect("encoded prompt is valid JSON");
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"], "hello");
    }

    #[test]
    fn encode_steer_emits_same_shape_as_prompt() {
        let parser = empty();
        let prompt = parser.encode_prompt("x").expect("encoder should succeed");
        let steer = parser.encode_steer("x").expect("encoder should succeed");
        assert_eq!(prompt, steer);
    }

    #[test]
    fn encode_abort_returns_none() {
        let parser = empty();
        let result = parser.encode_abort().expect("abort encoder should succeed");
        assert!(result.is_none());
    }

    // -- field-level coverage ----------------------------------------------

    /// `ClaudeMessage::Result` carries six fields. Pin every one — a renamed
    /// `total_cost_usd` (e.g. → `cost_total`) or a moved `is_error` flag
    /// would slip past the simpler success/error tests because they skip
    /// over the unused fields.
    #[test]
    fn result_message_round_trips_every_documented_field() {
        let line = r#"{"type":"result","subtype":"success","result":"ok body","total_cost_usd":1.25,"duration_ms":987,"num_turns":4,"is_error":false}"#;
        let msg: super::super::messages::ClaudeMessage = serde_json::from_str(line).expect("parse");
        match msg {
            super::super::messages::ClaudeMessage::Result {
                subtype,
                result,
                total_cost_usd,
                duration_ms,
                num_turns,
                is_error,
            } => {
                assert_eq!(subtype, "success");
                assert_eq!(result.as_deref(), Some("ok body"));
                assert_eq!(total_cost_usd, Some(1.25));
                assert_eq!(duration_ms, Some(987));
                assert_eq!(num_turns, Some(4));
                assert_eq!(is_error, Some(false));
            }
            other => panic!("expected Result, got {other:?}"),
        }
    }

    /// `ClaudeMessage::System` carries `subtype` (always present) and an
    /// optional `session_id`. Pin both — the parser logs `session_id` at
    /// info; a silent rename would erase the audit trail.
    #[test]
    fn system_message_maps_subtype_and_session_id() {
        let line = r#"{"type":"system","subtype":"init","session_id":"sess-xyz"}"#;
        let msg: super::super::messages::ClaudeMessage = serde_json::from_str(line).expect("parse");
        match msg {
            super::super::messages::ClaudeMessage::System {
                subtype,
                session_id,
            } => {
                assert_eq!(subtype, "init");
                assert_eq!(session_id.expect("session_id present").as_str(), "sess-xyz");
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    /// `ClaudeMessage::ControlRequest` field mapping: `id`, `tool`, `input`.
    /// Already exercised by `control_request_autoapproves_*` via parser
    /// behavior; this version pins the typed shape so a renamed field
    /// fails deserialization rather than silently changing the response.
    #[test]
    fn control_request_message_round_trips_all_fields() {
        let line =
            r#"{"type":"control_request","id":"req-7","tool":"Read","input":{"path":"/etc/x"}}"#;
        let msg: super::super::messages::ClaudeMessage = serde_json::from_str(line).expect("parse");
        match msg {
            super::super::messages::ClaudeMessage::ControlRequest { id, tool, input } => {
                assert_eq!(id.as_str(), "req-7");
                assert_eq!(tool, "Read");
                assert_eq!(input["path"], "/etc/x");
            }
            other => panic!("expected ControlRequest, got {other:?}"),
        }
    }

    /// `AssistantBlock::ToolUse` and `Text` field-level: pin each documented
    /// field (`text`, `id`, `name`, `input`).
    #[test]
    fn assistant_block_text_and_tool_use_field_mapping() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"abc"},{"type":"tool_use","id":"tu-1","name":"Bash","input":{"cmd":"ls"}}]}}"#;
        let p = parse(&empty(), line);
        assert_eq!(p.events.len(), 2);
        match &p.events[0] {
            AgentEvent::MessageDelta { text } => assert_eq!(text, "abc"),
            other => panic!("expected MessageDelta, got {other:?}"),
        }
        match &p.events[1] {
            AgentEvent::ToolCall { id, tool, params } => {
                assert_eq!(id.as_str(), "tu-1");
                assert_eq!(tool, "Bash");
                assert_eq!(params["cmd"], "ls");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    /// `UserBlock::ToolResult` field-level: pin `tool_use_id`, `content`,
    /// `is_error`. Non-string `content` is stringified per the parser
    /// contract — covers the structured-output codepath.
    #[test]
    fn user_block_tool_result_field_mapping() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu-2","content":{"k":"v"},"is_error":true}]}}"#;
        let p = parse(&empty(), line);
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            AgentEvent::ToolResult {
                id,
                output,
                is_error,
            } => {
                assert_eq!(id.as_str(), "tu-2");
                assert!(output.contains("\"k\""));
                assert!(*is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    /// Truncated JSON returns `ProtocolError::InvalidJson` — already covered
    /// by the pi parser; mirror it for claude so both backends share the
    /// malformed-line contract.
    #[test]
    fn malformed_json_returns_invalid_json_error() {
        let parser = empty();
        let err = parser
            .parse_line(r#"{"type":"assistant","message":{"role":"#)
            .err()
            .expect("malformed JSON should fail");
        assert!(matches!(err, ProtocolError::InvalidJson(_)));
    }
}
