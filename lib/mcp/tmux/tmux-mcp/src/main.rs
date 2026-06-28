//! MCP server for tmux pane management
//!
//! This is the main entry point for the tmux-mcp server.
//! It implements the MCP protocol over stdio and wires MCP tool calls
//! to the underlying tmux and pane management systems.

mod audit;
mod mcp;
mod panes;
mod tmux;

use audit::MaybeAuditLogger;
use mcp::{
    INTERNAL_ERROR, INVALID_PARAMS, JsonRpcResponse, METHOD_NOT_FOUND, McpHandler, McpMethod,
    RequestId, ToolCallParams, ToolCallResult,
};
use panes::{PaneManager, PaneStatus};
use serde::Serialize;
use serde_json::Value;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use tmux::{CommandExecutor, RealExecutor, TmuxSession};

/// Global flag to signal shutdown
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Application state shared across tool handlers
struct AppState<E: CommandExecutor = RealExecutor> {
    /// MCP protocol handler
    mcp_handler: McpHandler,
    /// Pane state manager
    pane_manager: PaneManager,
    /// Tmux session manager
    tmux_session: TmuxSession<E>,
    /// Optional audit logger
    audit: MaybeAuditLogger,
}

impl AppState<RealExecutor> {
    fn new() -> Self {
        Self {
            mcp_handler: McpHandler::new(),
            pane_manager: PaneManager::new(),
            tmux_session: TmuxSession::new(),
            audit: MaybeAuditLogger::from_env(),
        }
    }
}

/// Handle a tools/call request by dispatching to the appropriate tool handler
fn handle_tool_call<E: CommandExecutor>(
    state: &mut AppState<E>,
    params: &ToolCallParams,
) -> ToolCallResult {
    match params.name.as_str() {
        "tmux_create_pane" => handle_create_pane(state, &params.arguments),
        "tmux_send_keys" => handle_send_keys(state, &params.arguments),
        "tmux_capture_pane" => handle_capture_pane(state, &params.arguments),
        "tmux_kill_pane" => handle_kill_pane(state, &params.arguments),
        "tmux_list_panes" => handle_list_panes(state),
        _ => ToolCallResult::error(format!(
            "Unknown tool '{}'. Available tools: tmux_create_pane, tmux_send_keys, \
             tmux_capture_pane, tmux_kill_pane, tmux_list_panes",
            params.name
        )),
    }
}

fn audit_failure(tool: &str, error: impl std::fmt::Display) -> ToolCallResult {
    ToolCallResult::error(format!(
        "{tool} completed, but audit logging failed: {error}"
    ))
}

fn json_success<T: Serialize>(id: Option<RequestId>, result: T) -> JsonRpcResponse {
    match serde_json::to_value(result) {
        Ok(value) => JsonRpcResponse::success(id, value),
        Err(error) => JsonRpcResponse::error(
            id,
            INTERNAL_ERROR,
            format!("Failed to serialize response: {error}"),
        ),
    }
}

/// Handle `tmux_create_pane` tool call
fn handle_create_pane<E: CommandExecutor>(
    state: &mut AppState<E>,
    args: &std::collections::HashMap<String, Value>,
) -> ToolCallResult {
    // Extract required 'command' parameter
    let Some(command) = args.get("command").and_then(Value::as_str) else {
        return ToolCallResult::error(
            "Missing required parameter 'command'. Provide the command to run in the pane.",
        );
    };

    // Extract optional 'name' parameter
    let name = args.get("name").and_then(Value::as_str);

    // Register pane with PaneManager (generates unique ID)
    let pane_id = state.pane_manager.create_pane(command, name);

    // Create the actual tmux pane using the generated ID
    match state.tmux_session.create_pane(command, &pane_id) {
        Ok(_) => {
            if let Err(error) = state.audit.log_create_pane(&pane_id, command, name) {
                return audit_failure("tmux_create_pane", error);
            }

            let display_name = name.unwrap_or(&pane_id);
            ToolCallResult::success(format!(
                "Created pane '{}' (id: {}) running: {}",
                display_name, pane_id, command
            ))
        }
        Err(e) => {
            // Remove from pane manager on failure
            state.pane_manager.remove(&pane_id);
            ToolCallResult::error(format!("Failed to create pane: {}", e))
        }
    }
}

/// Handle `tmux_send_keys` tool call
fn handle_send_keys<E: CommandExecutor>(
    state: &AppState<E>,
    args: &std::collections::HashMap<String, Value>,
) -> ToolCallResult {
    // Extract required 'pane_id' parameter
    let Some(pane_id) = args.get("pane_id").and_then(Value::as_str) else {
        return ToolCallResult::error(
            "Missing required parameter 'pane_id'. Use tmux_list_panes to see active panes.",
        );
    };

    // Extract required 'keys' parameter
    let Some(keys) = args.get("keys").and_then(Value::as_str) else {
        return ToolCallResult::error(
            "Missing required parameter 'keys'. Provide the keystrokes to send.",
        );
    };

    // Verify pane exists in our tracking
    if !state.pane_manager.contains(pane_id) {
        return ToolCallResult::error(format!(
            "Pane '{}' not found. Use tmux_list_panes to see active panes.",
            pane_id
        ));
    }

    // Send keys to tmux
    match state.tmux_session.send_keys(pane_id, keys) {
        Ok(()) => {
            if let Err(error) = state.audit.log_send_keys(pane_id, keys) {
                return audit_failure("tmux_send_keys", error);
            }

            ToolCallResult::success(format!("Sent keys to pane '{}'", pane_id))
        }
        Err(e) => ToolCallResult::error(format!("Failed to send keys: {}", e)),
    }
}

/// Handle `tmux_capture_pane` tool call
fn handle_capture_pane<E: CommandExecutor>(
    state: &mut AppState<E>,
    args: &std::collections::HashMap<String, Value>,
) -> ToolCallResult {
    // Extract required 'pane_id' parameter
    let Some(pane_id) = args.get("pane_id").and_then(Value::as_str) else {
        return ToolCallResult::error(
            "Missing required parameter 'pane_id'. Use tmux_list_panes to see active panes.",
        );
    };

    // Extract optional 'lines' parameter (default 100, max 1000)
    let lines = args.get("lines").and_then(Value::as_i64).map_or(100, |n| {
        let clamped = n.clamp(1, 1000);
        i32::try_from(clamped).map_or(100, std::convert::identity)
    });

    // Verify pane exists in our tracking
    if !state.pane_manager.contains(pane_id) {
        return ToolCallResult::error(format!(
            "Pane '{}' not found. Use tmux_list_panes to see active panes.",
            pane_id
        ));
    }

    // Capture pane output from tmux
    match state.tmux_session.capture_pane(pane_id, lines) {
        Ok(output) => {
            let output_bytes = output.len();

            if let Err(error) = state.audit.log_capture_pane(pane_id, lines, output_bytes) {
                return audit_failure("tmux_capture_pane", error);
            }

            if let Err(error) = state.audit.save_full_capture(pane_id, &output) {
                return audit_failure("tmux_capture_pane full capture", error);
            }

            match state.tmux_session.get_window_info(pane_id) {
                Ok(info) => {
                    let new_status = if info.is_dead {
                        PaneStatus::Exited
                    } else {
                        PaneStatus::Running
                    };
                    state.pane_manager.update_status(pane_id, new_status);
                }
                Err(error) => {
                    return ToolCallResult::error(format!(
                        "Captured pane, but failed to refresh pane status: {error}"
                    ));
                }
            }

            ToolCallResult::success(output)
        }
        Err(e) => ToolCallResult::error(format!("Failed to capture pane: {}", e)),
    }
}

/// Handle `tmux_kill_pane` tool call
fn handle_kill_pane<E: CommandExecutor>(
    state: &mut AppState<E>,
    args: &std::collections::HashMap<String, Value>,
) -> ToolCallResult {
    // Extract required 'pane_id' parameter
    let Some(pane_id) = args.get("pane_id").and_then(Value::as_str) else {
        return ToolCallResult::error(
            "Missing required parameter 'pane_id'. Use tmux_list_panes to see active panes.",
        );
    };

    // Verify pane exists in our tracking
    if !state.pane_manager.contains(pane_id) {
        return ToolCallResult::error(format!(
            "Pane '{}' not found. Use tmux_list_panes to see active panes.",
            pane_id
        ));
    }

    // Kill the tmux pane
    match state.tmux_session.kill_pane(pane_id) {
        Ok(()) => {
            state.pane_manager.remove(pane_id);

            if let Err(error) = state.audit.log_kill_pane(pane_id) {
                return audit_failure("tmux_kill_pane", error);
            }

            ToolCallResult::success(format!("Killed pane '{}'", pane_id))
        }
        Err(e) => ToolCallResult::error(format!("Failed to kill pane: {}", e)),
    }
}

/// Handle `tmux_list_panes` tool call
fn handle_list_panes<E: CommandExecutor>(state: &mut AppState<E>) -> ToolCallResult {
    match state.tmux_session.list_windows() {
        Ok(windows) => {
            for window in windows {
                let status = if window.is_dead {
                    PaneStatus::Exited
                } else {
                    PaneStatus::Running
                };
                state.pane_manager.update_status(&window.name, status);
            }
        }
        Err(error) => {
            return ToolCallResult::error(format!(
                "Failed to refresh pane status before listing panes: {error}"
            ));
        }
    }

    if let Err(error) = state.audit.log_list_panes() {
        return audit_failure("tmux_list_panes", error);
    }

    // Build the list of panes
    let panes: Vec<serde_json::Value> = state
        .pane_manager
        .iter()
        .map(|pane| {
            serde_json::json!({
                "id": pane.id,
                "name": pane.name,
                "status": pane.status.as_str(),
                "command": pane.command
            })
        })
        .collect();

    if panes.is_empty() {
        ToolCallResult::success("No active panes. Use tmux_create_pane to create one.")
    } else {
        match serde_json::to_string_pretty(&panes) {
            Ok(json) => ToolCallResult::success(json),
            Err(error) => ToolCallResult::error(format!("Failed to serialize pane list: {error}")),
        }
    }
}

/// Process a single JSON-RPC request and return a response (if needed)
fn process_request<E: CommandExecutor>(
    state: &mut AppState<E>,
    line: &str,
) -> Option<JsonRpcResponse> {
    // Parse the request
    let request = match mcp::parse_request(line) {
        Ok(req) => req,
        Err(err_response) => return Some(*err_response),
    };

    // Parse the method
    let method = match McpMethod::from_request(&request) {
        Ok(m) => m,
        Err(e) => {
            return Some(JsonRpcResponse::error(request.id, INVALID_PARAMS, e));
        }
    };

    // Validate request against current state
    if let Err(e) = state.mcp_handler.validate_request(&method) {
        return Some(JsonRpcResponse::error(request.id, INTERNAL_ERROR, e));
    }

    // Handle the method
    match method {
        McpMethod::Initialize => {
            let result = state.mcp_handler.handle_initialize();
            Some(json_success(request.id, result))
        }
        McpMethod::Initialized => {
            state.mcp_handler.handle_initialized();
            // Notification - no response
            None
        }
        McpMethod::ToolsList => {
            let result = McpHandler::handle_tools_list();
            Some(json_success(request.id, result))
        }
        McpMethod::ToolsCall(params) => {
            let result = handle_tool_call(state, &params);
            Some(json_success(request.id, result))
        }
        McpMethod::Unknown(name) => Some(JsonRpcResponse::error(
            request.id,
            METHOD_NOT_FOUND,
            format!("Unknown method: {}", name),
        )),
    }
}

/// Set up signal handlers for graceful shutdown
fn setup_signal_handlers() -> io::Result<()> {
    #[cfg(unix)]
    {
        use signal_hook::consts::signal::{SIGINT, SIGTERM};

        let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT])?;
        std::thread::Builder::new()
            .name("tmux-mcp-signal-handler".to_string())
            .spawn(move || {
                if signals.forever().next().is_some() {
                    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
                }
            })
            .map(|_| ())
    }

    #[cfg(not(unix))]
    {
        Ok(())
    }
}

fn run_server_loop<E: CommandExecutor>(state: &mut AppState<E>) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
            break;
        }

        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        if let Some(response) = process_request(state, &line) {
            let response_json = mcp::serialize_response(&response);
            writeln!(stdout, "{}", response_json)?;
            stdout.flush()?;
        }
    }

    Ok(())
}

/// Main server loop - reads JSON-RPC requests from stdin, writes responses to stdout
fn run_server() -> io::Result<()> {
    setup_signal_handlers()?;

    let mut state = AppState::new();
    let loop_result = run_server_loop(&mut state);
    let cleanup_result = state.tmux_session.kill_session().map_err(io::Error::other);

    loop_result.and(cleanup_result)
}

fn main() -> io::Result<()> {
    run_server()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // Mock executor for tests
    struct MockExecutor;

    impl CommandExecutor for MockExecutor {
        fn execute(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
            let stdout = match args.first() {
                Some(&"display-message" | &"list-windows") => "debug-1|12345|0\n",
                Some(&"capture-pane") => "line 1\nline 2\nline 3\n",
                _ => "",
            };

            Ok(std::process::Output {
                status: std::process::ExitStatus::default(),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        }
    }

    // Helper to create test state with mock executor
    fn test_state() -> AppState<MockExecutor> {
        AppState {
            mcp_handler: McpHandler::new(),
            pane_manager: PaneManager::new(),
            tmux_session: TmuxSession::with_executor(MockExecutor),
            audit: MaybeAuditLogger::disabled(),
        }
    }

    // --- Initialize/Protocol Tests ---

    #[test]
    fn test_process_initialize_request() {
        let mut state = test_state();
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;

        let response = process_request(&mut state, request).unwrap();

        assert!(response.result.is_some());
        assert!(response.error.is_none());
        let result = response.result.unwrap();
        assert!(result.get("protocolVersion").is_some());
        assert!(result.get("serverInfo").is_some());
    }

    #[test]
    fn test_process_tools_list_request() {
        let mut state = test_state();

        // Initialize first
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        process_request(&mut state, init);

        // Then tools/list
        let request = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let response = process_request(&mut state, request).unwrap();

        assert!(response.result.is_some());
        let result = response.result.unwrap();
        let tools = result.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools.len(), 5);
    }

    #[test]
    fn test_process_tools_list_before_init_fails() {
        let mut state = test_state();

        let request = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let response = process_request(&mut state, request).unwrap();

        assert!(response.error.is_some());
        assert!(response.error.unwrap().message.contains("not initialized"));
    }

    #[test]
    fn test_process_initialized_notification() {
        let mut state = test_state();

        // Initialize first
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        process_request(&mut state, init);

        // Initialized is a notification - no response
        let request = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let response = process_request(&mut state, request);

        assert!(response.is_none());
    }

    #[test]
    fn test_process_unknown_method() {
        let mut state = test_state();

        let request = r#"{"jsonrpc":"2.0","id":1,"method":"unknown/method"}"#;
        let response = process_request(&mut state, request).unwrap();

        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, METHOD_NOT_FOUND);
    }

    #[test]
    fn test_process_invalid_json() {
        let mut state = test_state();

        let request = "not valid json";
        let response = process_request(&mut state, request).unwrap();

        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, mcp::PARSE_ERROR);
    }

    // --- Tool Call Tests ---

    #[test]
    fn test_handle_create_pane_success() {
        let mut state = test_state();

        let mut args = HashMap::new();
        args.insert(
            "command".to_string(),
            Value::String("cargo run".to_string()),
        );
        args.insert("name".to_string(), Value::String("server".to_string()));

        let result = handle_create_pane(&mut state, &args);

        assert!(!result.is_error);
        assert!(result.content[0].text.contains("Created pane"));
        assert!(result.content[0].text.contains("debug-1"));

        // Verify pane was added to manager
        assert!(state.pane_manager.contains("debug-1"));
    }

    #[test]
    fn test_handle_create_pane_missing_command() {
        let mut state = test_state();

        let args = HashMap::new();
        let result = handle_create_pane(&mut state, &args);

        assert!(result.is_error);
        assert!(
            result.content[0]
                .text
                .contains("Missing required parameter")
        );
    }

    #[test]
    fn test_handle_send_keys_success() {
        let mut state = test_state();

        // Create a pane first
        let mut create_args = HashMap::new();
        create_args.insert("command".to_string(), Value::String("bash".to_string()));
        handle_create_pane(&mut state, &create_args);

        // Send keys
        let mut args = HashMap::new();
        args.insert("pane_id".to_string(), Value::String("debug-1".to_string()));
        args.insert("keys".to_string(), Value::String("echo hello".to_string()));

        let result = handle_send_keys(&state, &args);

        assert!(!result.is_error);
        assert!(result.content[0].text.contains("Sent keys"));
    }

    #[test]
    fn test_handle_send_keys_pane_not_found() {
        let state = test_state();

        let mut args = HashMap::new();
        args.insert(
            "pane_id".to_string(),
            Value::String("nonexistent".to_string()),
        );
        args.insert("keys".to_string(), Value::String("echo hello".to_string()));

        let result = handle_send_keys(&state, &args);

        assert!(result.is_error);
        assert!(result.content[0].text.contains("not found"));
    }

    #[test]
    fn test_handle_send_keys_missing_pane_id() {
        let state = test_state();

        let mut args = HashMap::new();
        args.insert("keys".to_string(), Value::String("echo hello".to_string()));

        let result = handle_send_keys(&state, &args);

        assert!(result.is_error);
        assert!(
            result.content[0]
                .text
                .contains("Missing required parameter")
        );
    }

    #[test]
    fn test_handle_capture_pane_success() {
        let mut state = test_state();

        // Create a pane first
        let mut create_args = HashMap::new();
        create_args.insert("command".to_string(), Value::String("bash".to_string()));
        handle_create_pane(&mut state, &create_args);

        // Capture pane
        let mut args = HashMap::new();
        args.insert("pane_id".to_string(), Value::String("debug-1".to_string()));
        args.insert("lines".to_string(), Value::Number(50.into()));

        let result = handle_capture_pane(&mut state, &args);

        assert!(!result.is_error);
        // Mock returns "line 1\nline 2\nline 3\n"
        assert!(result.content[0].text.contains("line 1"));
    }

    #[test]
    fn test_handle_capture_pane_default_lines() {
        let mut state = test_state();

        // Create a pane first
        let mut create_args = HashMap::new();
        create_args.insert("command".to_string(), Value::String("bash".to_string()));
        handle_create_pane(&mut state, &create_args);

        // Capture pane without lines parameter (should default to 100)
        let mut args = HashMap::new();
        args.insert("pane_id".to_string(), Value::String("debug-1".to_string()));

        let result = handle_capture_pane(&mut state, &args);

        assert!(!result.is_error);
    }

    #[test]
    fn test_handle_capture_pane_clamps_lines() {
        let mut state = test_state();

        // Create a pane first
        let mut create_args = HashMap::new();
        create_args.insert("command".to_string(), Value::String("bash".to_string()));
        handle_create_pane(&mut state, &create_args);

        // Request more than max (1000)
        let mut args = HashMap::new();
        args.insert("pane_id".to_string(), Value::String("debug-1".to_string()));
        args.insert("lines".to_string(), Value::Number(5000.into()));

        let result = handle_capture_pane(&mut state, &args);

        // Should succeed (lines clamped to 1000)
        assert!(!result.is_error);
    }

    #[test]
    fn test_handle_kill_pane_success() {
        let mut state = test_state();

        // Create a pane first
        let mut create_args = HashMap::new();
        create_args.insert("command".to_string(), Value::String("bash".to_string()));
        handle_create_pane(&mut state, &create_args);

        assert!(state.pane_manager.contains("debug-1"));

        // Kill pane
        let mut args = HashMap::new();
        args.insert("pane_id".to_string(), Value::String("debug-1".to_string()));

        let result = handle_kill_pane(&mut state, &args);

        assert!(!result.is_error);
        assert!(result.content[0].text.contains("Killed pane"));

        // Verify pane was removed
        assert!(!state.pane_manager.contains("debug-1"));
    }

    #[test]
    fn test_handle_kill_pane_not_found() {
        let mut state = test_state();

        let mut args = HashMap::new();
        args.insert(
            "pane_id".to_string(),
            Value::String("nonexistent".to_string()),
        );

        let result = handle_kill_pane(&mut state, &args);

        assert!(result.is_error);
        assert!(result.content[0].text.contains("not found"));
    }

    #[test]
    fn test_handle_list_panes_empty() {
        let mut state = test_state();

        let result = handle_list_panes(&mut state);

        assert!(!result.is_error);
        assert!(result.content[0].text.contains("No active panes"));
    }

    #[test]
    fn test_handle_list_panes_with_panes() {
        let mut state = test_state();

        // Create some panes
        let mut args1 = HashMap::new();
        args1.insert(
            "command".to_string(),
            Value::String("cargo run".to_string()),
        );
        args1.insert("name".to_string(), Value::String("server".to_string()));
        handle_create_pane(&mut state, &args1);

        let mut args2 = HashMap::new();
        args2.insert("command".to_string(), Value::String("bash".to_string()));
        args2.insert("name".to_string(), Value::String("client".to_string()));
        handle_create_pane(&mut state, &args2);

        let result = handle_list_panes(&mut state);

        assert!(!result.is_error);
        // Should be valid JSON array
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&result.content[0].text).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_handle_unknown_tool() {
        let mut state = test_state();

        let params = ToolCallParams {
            name: "unknown_tool".to_string(),
            arguments: HashMap::new(),
        };

        let result = handle_tool_call(&mut state, &params);

        assert!(result.is_error);
        assert!(result.content[0].text.contains("Unknown tool"));
    }

    // --- Full Request Flow Tests ---

    #[test]
    fn test_full_create_pane_request() {
        let mut state = test_state();

        // Initialize
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        process_request(&mut state, init);

        // Create pane via tools/call
        let request = r#"{
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "tmux_create_pane",
                "arguments": {
                    "command": "RUST_LOG=debug cargo run",
                    "name": "server"
                }
            }
        }"#;

        let response = process_request(&mut state, request).unwrap();

        assert!(response.result.is_some());
        let result = response.result.unwrap();
        let content = result.get("content").unwrap().as_array().unwrap();
        assert!(!content.is_empty());

        // Check isError is not present or false
        assert!(
            result.get("isError").is_none() || !result.get("isError").unwrap().as_bool().unwrap()
        );
    }

    #[test]
    fn test_full_workflow() {
        let mut state = test_state();

        // Initialize
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        process_request(&mut state, init);

        // Create pane
        let create = r#"{
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "tmux_create_pane",
                "arguments": {"command": "bash", "name": "test"}
            }
        }"#;
        let resp = process_request(&mut state, create).unwrap();
        assert!(resp.error.is_none());

        // List panes
        let list = r#"{
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "tmux_list_panes", "arguments": {}}
        }"#;
        let resp = process_request(&mut state, list).unwrap();
        assert!(resp.error.is_none());

        // Send keys
        let send = r#"{
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "tmux_send_keys",
                "arguments": {"pane_id": "debug-1", "keys": "echo hello"}
            }
        }"#;
        let resp = process_request(&mut state, send).unwrap();
        assert!(resp.error.is_none());

        // Capture pane
        let capture = r#"{
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "tmux_capture_pane",
                "arguments": {"pane_id": "debug-1", "lines": 50}
            }
        }"#;
        let resp = process_request(&mut state, capture).unwrap();
        assert!(resp.error.is_none());

        // Kill pane
        let kill = r#"{
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "tmux_kill_pane",
                "arguments": {"pane_id": "debug-1"}
            }
        }"#;
        let resp = process_request(&mut state, kill).unwrap();
        assert!(resp.error.is_none());

        // Verify pane is gone
        assert!(!state.pane_manager.contains("debug-1"));
    }
}
