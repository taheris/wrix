//! MCP protocol handling (JSON-RPC over stdio)

use crate::pane::{PaneId, PaneIdError};
use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use std::collections::HashMap;
use thiserror::Error;

/// JSON-RPC request ID
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

/// JSON-RPC 2.0 Request
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<RequestId>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 Response
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    /// The request ID. Always serialized (as null if unknown) per JSON-RPC 2.0 spec.
    pub id: Option<RequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 Error
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    pub fn success(id: Option<RequestId>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<RequestId>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// Standard JSON-RPC error codes
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

// --- MCP Protocol Types ---

/// MCP server capabilities
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolsCapability {}

/// MCP server info
#[derive(Debug, Clone, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// MCP initialize response
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
}

/// MCP tool definition
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: ToolName,
    pub description: String,
    pub input_schema: InputSchema,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    pub properties: HashMap<String, PropertyDefinition>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PropertyDefinition {
    #[serde(rename = "type")]
    pub prop_type: String,
    pub description: String,
}

/// Supported tmux MCP tool names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolName {
    CreatePane,
    SendKeys,
    CapturePane,
    KillPane,
    ListPanes,
    Unknown(String),
}

impl ToolName {
    pub const CREATE_PANE: &'static str = "tmux_create_pane";
    pub const SEND_KEYS: &'static str = "tmux_send_keys";
    pub const CAPTURE_PANE: &'static str = "tmux_capture_pane";
    pub const KILL_PANE: &'static str = "tmux_kill_pane";
    pub const LIST_PANES: &'static str = "tmux_list_panes";

    pub fn from_wire(name: String) -> Self {
        match name.as_str() {
            Self::CREATE_PANE => Self::CreatePane,
            Self::SEND_KEYS => Self::SendKeys,
            Self::CAPTURE_PANE => Self::CapturePane,
            Self::KILL_PANE => Self::KillPane,
            Self::LIST_PANES => Self::ListPanes,
            _ => Self::Unknown(name),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::CreatePane => Self::CREATE_PANE,
            Self::SendKeys => Self::SEND_KEYS,
            Self::CapturePane => Self::CAPTURE_PANE,
            Self::KillPane => Self::KILL_PANE,
            Self::ListPanes => Self::LIST_PANES,
            Self::Unknown(name) => name,
        }
    }
}

impl std::fmt::Display for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ToolName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ToolName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        Ok(Self::from_wire(name))
    }
}

/// MCP tools/list response
#[derive(Debug, Clone, Serialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDefinition>,
}

/// JSON-RPC-level parse failure for a `tools/call` request.
#[derive(Debug, Display, Error)]
pub enum ToolCallParseError {
    /// tools/call requires params
    MissingParams,
    /// tools/call params must be an object
    ParamsNotObject,
    /// tools/call requires string parameter 'name'
    MissingName,
    /// tools/call parameter 'name' must be a string
    InvalidName,
}

/// Tool argument parse failure reported through MCP's tool-error envelope.
#[derive(Debug, Clone, Display, Error, PartialEq, Eq)]
pub enum ToolInputError {
    /// Invalid parameter `arguments`: expected an object.
    InvalidArguments,
    /// Missing required parameter `command`. Provide the command to run in the pane.
    MissingCommand,
    /// Invalid parameter `command`: expected a string.
    InvalidCommand,
    /// Invalid parameter `name`: expected a string.
    InvalidName,
    /// Missing required parameter `pane_id`. Use `tmux_list_panes` to see active panes.
    MissingPaneId,
    /// Invalid parameter `pane_id`: expected a string. Use `tmux_list_panes` to see active panes.
    InvalidPaneIdType,
    /// Invalid parameter `pane_id`: {source}. Use `tmux_list_panes` to see active panes.
    InvalidPaneId { source: PaneIdError },
    /// Missing required parameter `keys`. Provide the keystrokes to send.
    MissingKeys,
    /// Invalid parameter `keys`: expected a string.
    InvalidKeys,
    /// Invalid parameter `lines`: expected an integer between 1 and 1000.
    InvalidLines,
}

#[derive(Debug, Clone, Copy)]
enum ArgumentKey {
    Command,
    Name,
    PaneId,
    Keys,
    Lines,
}

impl ArgumentKey {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Name => "name",
            Self::PaneId => "pane_id",
            Self::Keys => "keys",
            Self::Lines => "lines",
        }
    }
}

/// Number of lines requested for `tmux_capture_pane`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureLines(i32);

impl CaptureLines {
    const DEFAULT: Self = Self(100);

    fn from_value(value: Option<&Value>) -> Result<Self, ToolInputError> {
        let Some(value) = value else {
            return Ok(Self::DEFAULT);
        };

        let Some(lines) = value.as_i64() else {
            return Err(ToolInputError::InvalidLines);
        };

        let clamped = lines.clamp(1, 1000);
        let lines = i32::try_from(clamped).map_err(|_error| ToolInputError::InvalidLines)?;
        Ok(Self(lines))
    }

    pub const fn as_i32(self) -> i32 {
        self.0
    }

    #[cfg(test)]
    pub const fn from_i32_for_test(value: i32) -> Self {
        Self(value)
    }
}

impl Default for CaptureLines {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Arguments for `tmux_create_pane`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePaneArgs {
    pub command: String,
    pub name: Option<String>,
}

/// Arguments for `tmux_send_keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendKeysArgs {
    pub pane_id: PaneId,
    pub keys: String,
}

/// Arguments for `tmux_capture_pane`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturePaneArgs {
    pub pane_id: PaneId,
    pub lines: CaptureLines,
}

/// Arguments for `tmux_kill_pane`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillPaneArgs {
    pub pane_id: PaneId,
}

/// Parsed MCP tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    CreatePane(CreatePaneArgs),
    SendKeys(SendKeysArgs),
    CapturePane(CapturePaneArgs),
    KillPane(KillPaneArgs),
    ListPanes,
    Invalid(ToolInputError),
    Unknown(String),
}

/// MCP tool call parameters parsed into tool-specific argument types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallParams {
    pub call: ToolCall,
}

impl ToolCallParams {
    pub fn from_value(value: &Value) -> Result<Self, ToolCallParseError> {
        let params = value
            .as_object()
            .ok_or(ToolCallParseError::ParamsNotObject)?;
        let Some(name_value) = params.get("name") else {
            return Err(ToolCallParseError::MissingName);
        };
        let Some(name) = name_value.as_str() else {
            return Err(ToolCallParseError::InvalidName);
        };

        let call = match ToolName::from_wire(name.to_string()) {
            ToolName::CreatePane => Self::tool_arguments(params)
                .and_then(Self::parse_create_pane)
                .map_or_else(ToolCall::Invalid, ToolCall::CreatePane),
            ToolName::SendKeys => Self::tool_arguments(params)
                .and_then(Self::parse_send_keys)
                .map_or_else(ToolCall::Invalid, ToolCall::SendKeys),
            ToolName::CapturePane => Self::tool_arguments(params)
                .and_then(Self::parse_capture_pane)
                .map_or_else(ToolCall::Invalid, ToolCall::CapturePane),
            ToolName::KillPane => Self::tool_arguments(params)
                .and_then(Self::parse_kill_pane)
                .map_or_else(ToolCall::Invalid, ToolCall::KillPane),
            ToolName::ListPanes => {
                Self::tool_arguments(params).map_or_else(ToolCall::Invalid, |_| ToolCall::ListPanes)
            }
            ToolName::Unknown(name) => ToolCall::Unknown(name),
        };

        Ok(Self { call })
    }

    fn tool_arguments(
        params: &Map<String, Value>,
    ) -> Result<Option<&Map<String, Value>>, ToolInputError> {
        match params.get("arguments") {
            Some(Value::Object(arguments)) => Ok(Some(arguments)),
            Some(_) => Err(ToolInputError::InvalidArguments),
            None => Ok(None),
        }
    }

    fn required_string(
        arguments: Option<&Map<String, Value>>,
        key: ArgumentKey,
        missing: ToolInputError,
        invalid: ToolInputError,
    ) -> Result<String, ToolInputError> {
        let Some(value) = arguments.and_then(|arguments| arguments.get(key.as_str())) else {
            return Err(missing);
        };
        let Some(value) = value.as_str() else {
            return Err(invalid);
        };
        Ok(value.to_string())
    }

    fn optional_string(
        arguments: Option<&Map<String, Value>>,
        key: ArgumentKey,
        invalid: ToolInputError,
    ) -> Result<Option<String>, ToolInputError> {
        let Some(value) = arguments.and_then(|arguments| arguments.get(key.as_str())) else {
            return Ok(None);
        };
        let Some(value) = value.as_str() else {
            return Err(invalid);
        };
        Ok(Some(value.to_string()))
    }

    fn required_pane_id(arguments: Option<&Map<String, Value>>) -> Result<PaneId, ToolInputError> {
        let raw = Self::required_string(
            arguments,
            ArgumentKey::PaneId,
            ToolInputError::MissingPaneId,
            ToolInputError::InvalidPaneIdType,
        )?;
        PaneId::parse(raw).map_err(|source| ToolInputError::InvalidPaneId { source })
    }

    fn parse_create_pane(
        arguments: Option<&Map<String, Value>>,
    ) -> Result<CreatePaneArgs, ToolInputError> {
        Ok(CreatePaneArgs {
            command: Self::required_string(
                arguments,
                ArgumentKey::Command,
                ToolInputError::MissingCommand,
                ToolInputError::InvalidCommand,
            )?,
            name: Self::optional_string(arguments, ArgumentKey::Name, ToolInputError::InvalidName)?,
        })
    }

    fn parse_send_keys(
        arguments: Option<&Map<String, Value>>,
    ) -> Result<SendKeysArgs, ToolInputError> {
        Ok(SendKeysArgs {
            pane_id: Self::required_pane_id(arguments)?,
            keys: Self::required_string(
                arguments,
                ArgumentKey::Keys,
                ToolInputError::MissingKeys,
                ToolInputError::InvalidKeys,
            )?,
        })
    }

    fn parse_capture_pane(
        arguments: Option<&Map<String, Value>>,
    ) -> Result<CapturePaneArgs, ToolInputError> {
        let lines = CaptureLines::from_value(
            arguments.and_then(|arguments| arguments.get(ArgumentKey::Lines.as_str())),
        )?;
        Ok(CapturePaneArgs {
            pane_id: Self::required_pane_id(arguments)?,
            lines,
        })
    }

    fn parse_kill_pane(
        arguments: Option<&Map<String, Value>>,
    ) -> Result<KillPaneArgs, ToolInputError> {
        Ok(KillPaneArgs {
            pane_id: Self::required_pane_id(arguments)?,
        })
    }
}

/// MCP tool call result content
#[derive(Debug, Clone, Serialize)]
pub struct TextContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

impl TextContent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            content_type: "text".to_string(),
            text: text.into(),
        }
    }
}

/// MCP tool call result
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    pub content: Vec<TextContent>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

impl ToolCallResult {
    pub fn success(text: impl Into<String>) -> Self {
        Self {
            content: vec![TextContent::new(text)],
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![TextContent::new(text)],
            is_error: true,
        }
    }
}

// --- Tool Definitions ---

/// Returns the list of available MCP tools
pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: ToolName::CreatePane,
            description: "Create a new tmux pane running a command. Use for spawning servers, \
                          test runners, or interactive shells. Returns a pane ID for subsequent \
                          operations."
                .to_string(),
            input_schema: InputSchema {
                schema_type: "object".to_string(),
                properties: {
                    let mut props = HashMap::new();
                    props.insert(
                        "command".to_string(),
                        PropertyDefinition {
                            prop_type: "string".to_string(),
                            description:
                                "Command to run in the pane (e.g., 'RUST_LOG=debug cargo run')"
                                    .to_string(),
                        },
                    );
                    props.insert(
                        "name".to_string(),
                        PropertyDefinition {
                            prop_type: "string".to_string(),
                            description: "Optional human-readable name for the pane".to_string(),
                        },
                    );
                    props
                },
                required: vec!["command".to_string()],
            },
        },
        ToolDefinition {
            name: ToolName::SendKeys,
            description: "Send keystrokes to a tmux pane. Use for interactive input, running \
                          additional commands, or sending signals (e.g., Ctrl-C as '^C')."
                .to_string(),
            input_schema: InputSchema {
                schema_type: "object".to_string(),
                properties: {
                    let mut props = HashMap::new();
                    props.insert(
                        "pane_id".to_string(),
                        PropertyDefinition {
                            prop_type: "string".to_string(),
                            description: "Target pane ID from tmux_create_pane or tmux_list_panes"
                                .to_string(),
                        },
                    );
                    props.insert(
                        "keys".to_string(),
                        PropertyDefinition {
                            prop_type: "string".to_string(),
                            description:
                                "Keystrokes to send. Use '^C' for Ctrl-C, 'Enter' for newline."
                                    .to_string(),
                        },
                    );
                    props
                },
                required: vec!["pane_id".to_string(), "keys".to_string()],
            },
        },
        ToolDefinition {
            name: ToolName::CapturePane,
            description: "Capture recent output from a tmux pane. Use to read logs, command \
                          output, or error messages. Works on both running and exited panes."
                .to_string(),
            input_schema: InputSchema {
                schema_type: "object".to_string(),
                properties: {
                    let mut props = HashMap::new();
                    props.insert(
                        "pane_id".to_string(),
                        PropertyDefinition {
                            prop_type: "string".to_string(),
                            description: "Target pane ID".to_string(),
                        },
                    );
                    props.insert(
                        "lines".to_string(),
                        PropertyDefinition {
                            prop_type: "number".to_string(),
                            description: "Number of lines to capture (default: 100, max: 1000)"
                                .to_string(),
                        },
                    );
                    props
                },
                required: vec!["pane_id".to_string()],
            },
        },
        ToolDefinition {
            name: ToolName::KillPane,
            description: "Terminate a tmux pane and its running process. Use for cleanup after \
                          debugging."
                .to_string(),
            input_schema: InputSchema {
                schema_type: "object".to_string(),
                properties: {
                    let mut props = HashMap::new();
                    props.insert(
                        "pane_id".to_string(),
                        PropertyDefinition {
                            prop_type: "string".to_string(),
                            description: "Target pane ID".to_string(),
                        },
                    );
                    props
                },
                required: vec!["pane_id".to_string()],
            },
        },
        ToolDefinition {
            name: ToolName::ListPanes,
            description: "List all active tmux panes with their IDs, names, status (running/\
                          exited), and running commands."
                .to_string(),
            input_schema: InputSchema {
                schema_type: "object".to_string(),
                properties: HashMap::new(),
                required: vec![],
            },
        },
    ]
}

// --- Request Routing ---

/// Parsed MCP method with typed parameters
#[derive(Debug)]
pub enum McpMethod {
    Initialize,
    Initialized,
    ToolsList,
    ToolsCall(ToolCallParams),
    Unknown(String),
}

impl McpMethod {
    /// Parse a JSON-RPC request into a typed MCP method
    pub fn from_request(request: &JsonRpcRequest) -> Result<Self, ToolCallParseError> {
        match request.method.as_str() {
            "initialize" => Ok(McpMethod::Initialize),
            "notifications/initialized" | "initialized" => Ok(McpMethod::Initialized),
            "tools/list" => Ok(McpMethod::ToolsList),
            "tools/call" => {
                let params = request
                    .params
                    .as_ref()
                    .ok_or(ToolCallParseError::MissingParams)?;
                Ok(McpMethod::ToolsCall(ToolCallParams::from_value(params)?))
            }
            other => Ok(McpMethod::Unknown(other.to_string())),
        }
    }
}

/// Protocol handler for MCP
pub struct McpHandler {
    initialized: bool,
}

impl McpHandler {
    pub const fn new() -> Self {
        Self { initialized: false }
    }

    /// Handle initialize request
    pub fn handle_initialize(&mut self) -> InitializeResult {
        self.initialized = true;
        InitializeResult {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {}),
            },
            server_info: ServerInfo {
                name: "tmux-mcp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        }
    }

    /// Handle initialized notification (no response needed)
    pub const fn handle_initialized(&mut self) {
        // Mark as fully initialized, ready for tool calls
        self.initialized = true;
    }

    /// Handle tools/list request
    pub fn handle_tools_list() -> ToolsListResult {
        ToolsListResult {
            tools: get_tool_definitions(),
        }
    }

    /// Check if a request is valid given current state
    pub const fn validate_request(&self, method: &McpMethod) -> Result<(), &'static str> {
        match method {
            McpMethod::ToolsList | McpMethod::ToolsCall(_) => {
                if self.initialized {
                    Ok(())
                } else {
                    Err("Server not initialized. Send 'initialize' first.")
                }
            }
            McpMethod::Initialize | McpMethod::Initialized | McpMethod::Unknown(_) => Ok(()),
        }
    }
}

impl Default for McpHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a line of input as a JSON-RPC request
pub fn parse_request(line: &str) -> Result<JsonRpcRequest, Box<JsonRpcResponse>> {
    let request: JsonRpcRequest = serde_json::from_str(line).map_err(|e| {
        Box::new(JsonRpcResponse::error(
            None,
            PARSE_ERROR,
            format!("Parse error: {e}"),
        ))
    })?;

    if request.jsonrpc == "2.0" {
        Ok(request)
    } else {
        Err(Box::new(JsonRpcResponse::error(
            request.id,
            INVALID_REQUEST,
            "Invalid JSON-RPC version. Expected '2.0'.",
        )))
    }
}

/// Serialize a response to a JSON string (single line)
pub fn serialize_response(response: &JsonRpcResponse) -> String {
    serde_json::to_string(response).unwrap_or_else(|e| {
        // Fallback error response if serialization fails
        format!(
            r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":{},"message":"Serialization error: {}"}}}}"#,
            INTERNAL_ERROR, e
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Request Parsing Tests ---

    #[test]
    fn test_parse_initialize_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let request = parse_request(json).unwrap();

        assert_eq!(request.jsonrpc, "2.0");
        assert_eq!(request.id, Some(RequestId::Number(1)));
        assert_eq!(request.method, "initialize");
    }

    #[test]
    fn test_parse_request_with_string_id() {
        let json = r#"{"jsonrpc":"2.0","id":"abc-123","method":"tools/list"}"#;
        let request = parse_request(json).unwrap();

        assert_eq!(request.id, Some(RequestId::String("abc-123".to_string())));
    }

    #[test]
    fn test_parse_tools_call_request() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/call",
            "params": {
                "name": "tmux_create_pane",
                "arguments": {
                    "command": "cargo run",
                    "name": "server"
                }
            }
        }"#;
        let request = parse_request(json).unwrap();

        assert_eq!(request.method, "tools/call");
        let method = McpMethod::from_request(&request).unwrap();
        match method {
            McpMethod::ToolsCall(params) => match params.call {
                ToolCall::CreatePane(args) => {
                    assert_eq!(args.command, "cargo run");
                    assert_eq!(args.name.as_deref(), Some("server"));
                }
                _ => panic!("Expected create_pane call"),
            },
            _ => panic!("Expected ToolsCall"),
        }
    }

    #[test]
    fn test_parse_tool_arguments_invalid_pane_id_stays_tool_error() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/call",
            "params": {
                "name": "tmux_send_keys",
                "arguments": {
                    "pane_id": "missing-pane",
                    "keys": "echo hello"
                }
            }
        }"#;
        let request = parse_request(json).unwrap();

        let method = McpMethod::from_request(&request).unwrap();
        match method {
            McpMethod::ToolsCall(params) => match params.call {
                ToolCall::Invalid(ToolInputError::InvalidPaneId { source }) => {
                    assert_eq!(
                        source.to_string(),
                        "Pane id 'missing-pane' must use debug-N with N greater than zero"
                    );
                }
                _ => panic!("Expected invalid pane id"),
            },
            _ => panic!("Expected ToolsCall"),
        }
    }

    #[test]
    fn test_parse_capture_lines_clamps_to_maximum() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/call",
            "params": {
                "name": "tmux_capture_pane",
                "arguments": {
                    "pane_id": "debug-1",
                    "lines": 5000
                }
            }
        }"#;
        let request = parse_request(json).unwrap();

        let method = McpMethod::from_request(&request).unwrap();
        match method {
            McpMethod::ToolsCall(params) => match params.call {
                ToolCall::CapturePane(args) => assert_eq!(args.lines.as_i32(), 1000),
                _ => panic!("Expected capture_pane call"),
            },
            _ => panic!("Expected ToolsCall"),
        }
    }

    #[test]
    fn test_parse_invalid_json() {
        let json = "not valid json";
        let response = parse_request(json).unwrap_err();

        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, PARSE_ERROR);
    }

    #[test]
    fn test_parse_notification_no_id() {
        let json = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let request = parse_request(json).unwrap();

        assert!(request.id.is_none());
        assert_eq!(request.method, "notifications/initialized");
    }

    // --- Response Serialization Tests ---

    #[test]
    fn test_serialize_success_response() {
        let response = JsonRpcResponse::success(
            Some(RequestId::Number(1)),
            serde_json::json!({"status": "ok"}),
        );
        let json = serialize_response(&response);

        assert!(json.contains(r#""jsonrpc":"2.0""#));
        assert!(json.contains(r#""id":1"#));
        assert!(json.contains(r#""result""#));
        assert!(!json.contains(r#""error""#));
    }

    #[test]
    fn test_serialize_error_response() {
        let response = JsonRpcResponse::error(
            Some(RequestId::Number(1)),
            METHOD_NOT_FOUND,
            "Method not found",
        );
        let json = serialize_response(&response);

        assert!(json.contains(r#""error""#));
        assert!(json.contains(r#""code":-32601"#));
        assert!(json.contains(r#""message":"Method not found""#));
        assert!(!json.contains(r#""result""#));
    }

    #[test]
    fn test_serialize_response_null_id() {
        let response = JsonRpcResponse::error(None, PARSE_ERROR, "Parse error");
        let json = serialize_response(&response);

        assert!(json.contains(r#""id":null"#));
    }

    // --- Tool Definitions Tests ---

    #[test]
    fn test_get_tool_definitions_count() {
        let tools = get_tool_definitions();
        assert_eq!(tools.len(), 5);
    }

    #[test]
    fn test_tool_definitions_names() {
        let tools = get_tool_definitions();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();

        assert!(names.contains(&"tmux_create_pane"));
        assert!(names.contains(&"tmux_send_keys"));
        assert!(names.contains(&"tmux_capture_pane"));
        assert!(names.contains(&"tmux_kill_pane"));
        assert!(names.contains(&"tmux_list_panes"));
    }

    #[test]
    fn test_tool_definition_create_pane_schema() {
        let tools = get_tool_definitions();
        let create_pane = tools
            .iter()
            .find(|t| t.name == ToolName::CreatePane)
            .unwrap();

        assert_eq!(create_pane.input_schema.schema_type, "object");
        assert!(create_pane.input_schema.properties.contains_key("command"));
        assert!(create_pane.input_schema.properties.contains_key("name"));
        assert!(
            create_pane
                .input_schema
                .required
                .contains(&"command".to_string())
        );
        assert!(
            !create_pane
                .input_schema
                .required
                .contains(&"name".to_string())
        );
    }

    #[test]
    fn test_tool_definition_list_panes_no_required() {
        let tools = get_tool_definitions();
        let list_panes = tools
            .iter()
            .find(|t| t.name == ToolName::ListPanes)
            .unwrap();

        assert!(list_panes.input_schema.properties.is_empty());
        assert!(list_panes.input_schema.required.is_empty());
    }

    // --- MCP Method Parsing Tests ---

    #[test]
    fn test_method_from_request_initialize() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(RequestId::Number(1)),
            method: "initialize".to_string(),
            params: None,
        };

        match McpMethod::from_request(&request).unwrap() {
            McpMethod::Initialize => {}
            _ => panic!("Expected Initialize"),
        }
    }

    #[test]
    fn test_method_from_request_tools_list() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(RequestId::Number(1)),
            method: "tools/list".to_string(),
            params: None,
        };

        match McpMethod::from_request(&request).unwrap() {
            McpMethod::ToolsList => {}
            _ => panic!("Expected ToolsList"),
        }
    }

    #[test]
    fn test_method_from_request_unknown() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(RequestId::Number(1)),
            method: "some/unknown/method".to_string(),
            params: None,
        };

        match McpMethod::from_request(&request).unwrap() {
            McpMethod::Unknown(m) => assert_eq!(m, "some/unknown/method"),
            _ => panic!("Expected Unknown"),
        }
    }

    #[test]
    fn test_tools_call_missing_params() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(RequestId::Number(1)),
            method: "tools/call".to_string(),
            params: None,
        };

        let result = McpMethod::from_request(&request);
        assert!(result.is_err());
    }

    // --- McpHandler Tests ---

    #[test]
    fn test_handler_initialize() {
        let mut handler = McpHandler::new();
        let result = handler.handle_initialize();

        assert_eq!(result.protocol_version, "2024-11-05");
        assert_eq!(result.server_info.name, "tmux-mcp");
        assert!(result.capabilities.tools.is_some());
    }

    #[test]
    fn test_handler_tools_list() {
        let result = McpHandler::handle_tools_list();

        assert_eq!(result.tools.len(), 5);
    }

    #[test]
    fn test_handler_validate_before_init() {
        let handler = McpHandler::new();

        // Initialize is always OK
        assert!(handler.validate_request(&McpMethod::Initialize).is_ok());

        // Tools calls require initialization
        let params = ToolCallParams {
            call: ToolCall::ListPanes,
        };
        assert!(
            handler
                .validate_request(&McpMethod::ToolsCall(params))
                .is_err()
        );
    }

    #[test]
    fn test_handler_validate_after_init() {
        let mut handler = McpHandler::new();
        handler.handle_initialize();

        // Now tools/list should work
        assert!(handler.validate_request(&McpMethod::ToolsList).is_ok());

        // And tools/call should work
        let params = ToolCallParams {
            call: ToolCall::ListPanes,
        };
        assert!(
            handler
                .validate_request(&McpMethod::ToolsCall(params))
                .is_ok()
        );
    }

    // --- ToolCallResult Tests ---

    #[test]
    fn test_tool_call_result_success() {
        let result = ToolCallResult::success("Pane created: debug-1");
        let json = serde_json::to_string(&result).unwrap();

        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains(r#""text":"Pane created: debug-1""#));
        assert!(!json.contains("isError"));
    }

    #[test]
    fn test_tool_call_result_error() {
        let result = ToolCallResult::error(
            "Pane 'debug-1' not found. Use tmux_list_panes to see active panes.",
        );
        let json = serde_json::to_string(&result).unwrap();

        assert!(json.contains(r#""isError":true"#));
        assert!(json.contains("not found"));
    }

    // --- Full Round-Trip Tests ---

    #[test]
    fn test_initialize_roundtrip() {
        let request_json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#;

        let request = parse_request(request_json).unwrap();
        let method = McpMethod::from_request(&request).unwrap();

        let mut handler = McpHandler::new();
        match method {
            McpMethod::Initialize => {
                let result = handler.handle_initialize();
                let response =
                    JsonRpcResponse::success(request.id, serde_json::to_value(result).unwrap());
                let response_json = serialize_response(&response);

                assert!(response_json.contains("protocolVersion"));
                assert!(response_json.contains("tmux-mcp"));
            }
            _ => panic!("Expected Initialize"),
        }
    }

    #[test]
    fn test_tools_list_roundtrip() {
        let request_json = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

        let request = parse_request(request_json).unwrap();
        let result = McpHandler::handle_tools_list();
        let response = JsonRpcResponse::success(request.id, serde_json::to_value(result).unwrap());
        let response_json = serialize_response(&response);

        // Verify all 5 tools are in the response
        assert!(response_json.contains("tmux_create_pane"));
        assert!(response_json.contains("tmux_send_keys"));
        assert!(response_json.contains("tmux_capture_pane"));
        assert!(response_json.contains("tmux_kill_pane"));
        assert!(response_json.contains("tmux_list_panes"));
    }
}
