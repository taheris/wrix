#!/usr/bin/env bash
# Shared test library for tmux-mcp integration tests
#
# Source this file in test scripts:
#   source "$(dirname "$0")/test_lib.sh"
#
# Provides:
# - MCP server lifecycle management (start_mcp_server, stop_mcp_server)
# - JSON-RPC request helpers (mcp_request, mcp_initialize, mcp_create_pane, etc.)
# - Assertion helpers (assert_eq, assert_contains, assert_json_field, etc.)
# - Cleanup handling (registers cleanup on EXIT)

set -euo pipefail

# Find repo root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
export REPO_ROOT

# Colors for output
export RED='\033[0;31m'
export GREEN='\033[0;32m'
export YELLOW='\033[1;33m'
export BLUE='\033[0;34m'
export NC='\033[0m' # No Color

# Test state
MCP_PID=""
MCP_FIFO_IN=""
MCP_FIFO_OUT=""
MCP_FD_IN=""
MCP_FD_OUT=""
TEMP_DIR=""
REQUEST_ID=0

# Logging functions
log_info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*"
}

log_debug() {
    if [[ "${VERBOSE:-0}" == "1" ]]; then
        echo -e "${BLUE}[DEBUG]${NC} $*" >&2
    fi
}

log_test() {
    echo -e "${BLUE}[TEST]${NC} $*"
}

log_pass() {
    echo -e "${GREEN}[PASS]${NC} $*"
}

log_fail() {
    echo -e "${RED}[FAIL]${NC} $*"
}

# Create temporary directory for test (only if not already set)
setup_temp_dir() {
    if [[ -z "${TEMP_DIR:-}" ]] || [[ ! -d "${TEMP_DIR}" ]]; then
        TEMP_DIR=$(mktemp -d)
        log_debug "Created temp dir: $TEMP_DIR"
    fi
}

# Cleanup function - called on EXIT
cleanup() {
    local exit_code=$?
    log_debug "Cleanup running (exit code: $exit_code)"

    # Stop MCP server if running
    stop_mcp_server 2>/dev/null || true # best-effort: cleanup may run before server startup completes.

    # Remove temp directory
    if [[ -n "${TEMP_DIR:-}" ]] && [[ -d "$TEMP_DIR" ]]; then
        rm -rf "$TEMP_DIR"
        log_debug "Removed temp dir: $TEMP_DIR"
    fi

    exit "$exit_code"
}
trap cleanup EXIT

# Find tmux-mcp binary
find_mcp_binary() {
    if command -v tmux-mcp &>/dev/null; then
        echo "tmux-mcp"
        return 0
    fi

    local cargo_bin="${REPO_ROOT}/target/debug/tmux-mcp"
    if [[ -x "$cargo_bin" ]]; then
        echo "$cargo_bin"
        return 0
    fi

    cargo_bin="${REPO_ROOT}/lib/mcp/tmux/tmux-mcp/target/debug/tmux-mcp"
    if [[ -x "$cargo_bin" ]]; then
        echo "$cargo_bin"
        return 0
    fi

    local release_bin="${REPO_ROOT}/target/release/tmux-mcp"
    if [[ -x "$release_bin" ]]; then
        echo "$release_bin"
        return 0
    fi

    release_bin="${REPO_ROOT}/lib/mcp/tmux/tmux-mcp/target/release/tmux-mcp"
    if [[ -x "$release_bin" ]]; then
        echo "$release_bin"
        return 0
    fi

    log_error "Cannot find tmux-mcp binary"
    return 1
}

# Start MCP server with named pipes for communication
# shellcheck disable=SC2120  # $@ is intentionally optional (env vars)
start_mcp_server() {
    local env_vars=("$@")

    setup_temp_dir

    MCP_FIFO_IN="${TEMP_DIR}/mcp_in"
    MCP_FIFO_OUT="${TEMP_DIR}/mcp_out"
    mkfifo "$MCP_FIFO_IN" "$MCP_FIFO_OUT"

    local mcp_bin
    mcp_bin=$(find_mcp_binary)

    # Start MCP server with optional environment variables
    if [[ ${#env_vars[@]} -gt 0 ]]; then
        env "${env_vars[@]}" "$mcp_bin" < "$MCP_FIFO_IN" > "$MCP_FIFO_OUT" 2>/dev/null &
    else
        "$mcp_bin" < "$MCP_FIFO_IN" > "$MCP_FIFO_OUT" 2>/dev/null &
    fi
    MCP_PID=$!

    # Give server time to start
    sleep 0.2

    # Verify server is running
    if ! kill -0 "$MCP_PID" 2>/dev/null; then
        log_error "MCP server failed to start"
        return 1
    fi

    # Open file descriptors for communication
    exec 3>"$MCP_FIFO_IN"
    exec 4<"$MCP_FIFO_OUT"
    MCP_FD_IN=3
    MCP_FD_OUT=4

    log_debug "Started MCP server (PID: $MCP_PID)"
    return 0
}

# Stop MCP server
stop_mcp_server() {
    if [[ -n "${MCP_FD_IN:-}" ]]; then
        exec 3>&- 2>/dev/null || true # best-effort: descriptor may already be closed by a prior cleanup.
        MCP_FD_IN=""
    fi
    if [[ -n "${MCP_FD_OUT:-}" ]]; then
        exec 4<&- 2>/dev/null || true # best-effort: descriptor may already be closed by a prior cleanup.
        MCP_FD_OUT=""
    fi
    if [[ -n "${MCP_PID:-}" ]]; then
        kill "$MCP_PID" 2>/dev/null || true # best-effort: server may have already exited.
        wait "$MCP_PID" 2>/dev/null || true # best-effort: child may already be reaped after signal handling.
        log_debug "Stopped MCP server (PID: $MCP_PID)"
        MCP_PID=""
    fi
    if [[ -n "${MCP_FIFO_IN:-}" ]]; then
        rm -f "$MCP_FIFO_IN"
        MCP_FIFO_IN=""
    fi
    if [[ -n "${MCP_FIFO_OUT:-}" ]]; then
        rm -f "$MCP_FIFO_OUT"
        MCP_FIFO_OUT=""
    fi
}

# Get next request ID
next_request_id() {
    ((REQUEST_ID++))
    echo "$REQUEST_ID"
}

# Send JSON-RPC request and get response
# Usage: mcp_request '<json>' [timeout_seconds]
mcp_request() {
    local request="$1"
    local timeout="${2:-5}"

    log_debug "Request: $request"

    echo "$request" >&3

    local response
    if ! read -r -t "$timeout" response <&4; then
        log_error "Timeout waiting for response"
        return 1
    fi

    log_debug "Response: $response"
    echo "$response"
}

# Send JSON-RPC notification (no response expected)
mcp_notify() {
    local request="$1"
    log_debug "Notification: $request"
    echo "$request" >&3
    # Small delay to ensure notification is processed
    sleep 0.1
}

# --- MCP Helper Functions ---

mcp_initialize() {
    local id
    id=$(next_request_id)
    mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1.0\"}}}"
}

mcp_initialized() {
    mcp_notify '{"jsonrpc":"2.0","method":"notifications/initialized"}'
}

mcp_list_tools() {
    local id
    id=$(next_request_id)
    mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/list\"}"
}

mcp_create_pane() {
    local command="$1"
    local name="${2:-}"
    local id
    id=$(next_request_id)

    local args="{\"command\":\"$command\""
    if [[ -n "$name" ]]; then
        args="$args,\"name\":\"$name\""
    fi
    args="$args}"

    mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"tmux_create_pane\",\"arguments\":$args}}"
}

mcp_send_keys() {
    local pane_id="$1"
    local keys="$2"
    local id
    id=$(next_request_id)
    mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"tmux_send_keys\",\"arguments\":{\"pane_id\":\"$pane_id\",\"keys\":\"$keys\"}}}"
}

mcp_capture_pane() {
    local pane_id="$1"
    local id
    local args
    id=$(next_request_id)
    args="{\"pane_id\":\"$pane_id\""
    if [[ $# -ge 2 ]]; then
        local lines="$2"
        args="$args,\"lines\":$lines"
    fi
    args="$args}"
    mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"tmux_capture_pane\",\"arguments\":$args}}"
}

mcp_kill_pane() {
    local pane_id="$1"
    local id
    id=$(next_request_id)
    mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"tmux_kill_pane\",\"arguments\":{\"pane_id\":\"$pane_id\"}}}"
}

mcp_list_panes() {
    local id
    id=$(next_request_id)
    mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"tmux_list_panes\",\"arguments\":{}}}"
}

# --- JSON Parsing Helpers ---

# Extract field from JSON using jq
json_get() {
    local json="$1"
    local path="$2"
    echo "$json" | jq -r "$path" 2>/dev/null
}

# Check if response has an error
response_has_error() {
    local response="$1"
    local is_error
    is_error=$(json_get "$response" '.result.isError // false')
    [[ "$is_error" == "true" ]]
}

# Check if response has a JSON-RPC error
response_has_jsonrpc_error() {
    local response="$1"
    local error
    error=$(json_get "$response" '.error // empty')
    [[ -n "$error" ]]
}

# Get content text from tool result
get_content_text() {
    local response="$1"
    json_get "$response" '.result.content[0].text'
}

# Extract pane ID from create_pane response
# The response text looks like "Created pane 'name' (id: debug-1) running: command"
extract_pane_id() {
    local response="$1"
    local text
    text=$(get_content_text "$response")
    echo "$text" | grep -oP '\(id: \K[^)]+' || echo ""
}

# --- Assertion Helpers ---

assert_eq() {
    local expected="$1"
    local actual="$2"
    local message="${3:-Values should be equal}"

    if [[ "$expected" == "$actual" ]]; then
        return 0
    else
        log_fail "$message"
        log_error "  Expected: $expected"
        log_error "  Actual:   $actual"
        return 1
    fi
}

assert_ne() {
    local unexpected="$1"
    local actual="$2"
    local message="${3:-Values should not be equal}"

    if [[ "$unexpected" != "$actual" ]]; then
        return 0
    else
        log_fail "$message"
        log_error "  Unexpected: $unexpected"
        log_error "  Actual:     $actual"
        return 1
    fi
}

assert_contains() {
    local haystack="$1"
    local needle="$2"
    local message="${3:-String should contain substring}"

    if [[ "$haystack" == *"$needle"* ]]; then
        return 0
    else
        log_fail "$message"
        log_error "  Looking for: $needle"
        log_error "  In string:   $haystack"
        return 1
    fi
}

assert_not_contains() {
    local haystack="$1"
    local needle="$2"
    local message="${3:-String should not contain substring}"

    if [[ "$haystack" != *"$needle"* ]]; then
        return 0
    else
        log_fail "$message"
        log_error "  Should not contain: $needle"
        log_error "  In string:          $haystack"
        return 1
    fi
}

assert_success() {
    local response="$1"
    local message="${2:-Response should indicate success}"

    if response_has_jsonrpc_error "$response"; then
        log_fail "$message"
        log_error "  Response has JSON-RPC error: $(json_get "$response" '.error.message')"
        return 1
    fi

    if response_has_error "$response"; then
        log_fail "$message"
        log_error "  Response has tool error: $(get_content_text "$response")"
        return 1
    fi

    return 0
}

assert_error() {
    local response="$1"
    local message="${2:-Response should indicate error}"

    if response_has_error "$response" || response_has_jsonrpc_error "$response"; then
        return 0
    else
        log_fail "$message"
        log_error "  Expected error, got success"
        log_error "  Response: $response"
        return 1
    fi
}

assert_json_field() {
    local json="$1"
    local path="$2"
    local expected="$3"
    local message="${4:-JSON field should match expected value}"

    local actual
    actual=$(json_get "$json" "$path")

    assert_eq "$expected" "$actual" "$message"
}

mcp_tmux() {
    local server_name="$1"
    local process_id="${server_name#debug-}"
    local socket_path="${TMPDIR:-/tmp}/tmux-mcp-${process_id}.sock"
    shift
    tmux -S "$socket_path" "$@"
}

# Check if tmux window exists
assert_tmux_window_exists() {
    local session="$1"
    local window="$2"
    local message="${3:-Tmux window should exist}"

    if mcp_tmux "$session" list-windows -t "$session" 2>/dev/null | grep -q "$window"; then
        return 0
    else
        log_fail "$message"
        log_error "  Session: $session"
        log_error "  Window:  $window"
        return 1
    fi
}

assert_tmux_window_not_exists() {
    local session="$1"
    local window="$2"
    local message="${3:-Tmux window should not exist}"

    if ! mcp_tmux "$session" list-windows -t "$session" 2>/dev/null | grep -q "$window"; then
        return 0
    else
        log_fail "$message"
        log_error "  Session: $session"
        log_error "  Window:  $window (should not exist)"
        return 1
    fi
}

# Check if tmux session exists
assert_tmux_session_exists() {
    local session="$1"
    local message="${2:-Tmux session should exist}"

    if mcp_tmux "$session" has-session -t "$session" 2>/dev/null; then
        return 0
    else
        log_fail "$message"
        log_error "  Session: $session (not found)"
        return 1
    fi
}

assert_tmux_session_not_exists() {
    local session="$1"
    local message="${2:-Tmux session should not exist}"

    if ! mcp_tmux "$session" has-session -t "$session" 2>/dev/null; then
        return 0
    else
        log_fail "$message"
        log_error "  Session: $session (should not exist)"
        return 1
    fi
}

# Get MCP server's tmux session name
get_mcp_session_name() {
    echo "debug-${MCP_PID}"
}

# Wait for a condition to be true (with timeout)
# Usage: wait_for <command> [timeout_seconds] [interval_seconds]
wait_for() {
    local cmd="$1"
    local timeout="${2:-5}"
    local interval="${3:-0.2}"

    local elapsed=0
    while ! eval "$cmd" 2>/dev/null; do
        sleep "$interval"
        elapsed=$(echo "$elapsed + $interval" | bc)
        if (( $(echo "$elapsed >= $timeout" | bc -l) )); then
            return 1
        fi
    done
    return 0
}

log_debug "Test library loaded"
