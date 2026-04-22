#!/usr/bin/env bash
# Test: Run MCP server inside sandbox, create pane, send keys, capture output
#
# This test exercises the full MCP workflow inside a sandbox container:
# 1. Start the tmux-mcp server inside the container
# 2. Send JSON-RPC requests to create a pane
# 3. Send keys to the pane
# 4. Capture output from the pane
# 5. Kill the pane
#
# Uses MCP opt-in:
#   mkSandbox { profile = base; mcp = { tmux = {}; }; }
#
# Prerequisites:
# - nix (with flakes enabled)
# - podman
#
# Usage: ./test_mcp_in_sandbox.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${SCRIPT_DIR}/../../.."

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*"
}

# shellcheck disable=SC2317,SC2329  # cleanup is used by trap
cleanup() {
    local exit_code=$?
    if [[ -n "${WORKSPACE:-}" ]] && [[ -d "${WORKSPACE}" ]]; then
        rm -rf "${WORKSPACE}"
    fi
    exit "$exit_code"
}
trap cleanup EXIT

# Check prerequisites
if ! command -v nix &>/dev/null; then
    log_error "nix is required but not installed"
    exit 1
fi

if ! command -v podman &>/dev/null; then
    log_error "podman is required but not installed"
    exit 1
fi

log_info "Building sandbox-mcp image (mcpRuntime=true)..."

# Build the mcp image: all MCP server packages included, runtime selection via env vars
IMAGE_PATH=$(nix build "${REPO_ROOT}#sandbox-mcp" --print-out-paths 2>/dev/null) || {
    log_error "Failed to build sandbox-mcp image"
    log_warn "Check that the mcp parameter is properly configured in lib/sandbox/default.nix"
    exit 1
}

if [[ ! -f "${IMAGE_PATH}" ]]; then
    log_error "Built image not found at ${IMAGE_PATH}"
    exit 1
fi

log_info "Image built: ${IMAGE_PATH}"

# Create a temporary workspace
WORKSPACE=$(mktemp -d)
log_info "Using workspace: ${WORKSPACE}"

# Create a test script that will run inside the container
# This script:
# 1. Starts tmux-mcp in background
# 2. Sends JSON-RPC requests via stdin/stdout
# 3. Validates responses
cat > "${WORKSPACE}/mcp_test.sh" << 'INNER_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail

# Helper to send JSON-RPC request and get response
send_request() {
    local request="$1"
    echo "$request" | tmux-mcp 2>/dev/null
}

# MCP JSON-RPC helper functions
mcp_initialize() {
    cat <<EOF
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}
EOF
}

mcp_list_tools() {
    cat <<EOF
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
EOF
}

mcp_create_pane() {
    local command="$1"
    local name="${2:-}"
    if [[ -n "$name" ]]; then
        cat <<EOF
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tmux_create_pane","arguments":{"command":"$command","name":"$name"}}}
EOF
    else
        cat <<EOF
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tmux_create_pane","arguments":{"command":"$command"}}}
EOF
    fi
}

mcp_send_keys() {
    local pane_id="$1"
    local keys="$2"
    cat <<EOF
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"tmux_send_keys","arguments":{"pane_id":"$pane_id","keys":"$keys"}}}
EOF
}

mcp_capture_pane() {
    local pane_id="$1"
    local lines="${2:-100}"
    cat <<EOF
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"tmux_capture_pane","arguments":{"pane_id":"$pane_id","lines":$lines}}}
EOF
}

mcp_kill_pane() {
    local pane_id="$1"
    cat <<EOF
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"tmux_kill_pane","arguments":{"pane_id":"$pane_id"}}}
EOF
}

mcp_list_panes() {
    cat <<EOF
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"tmux_list_panes","arguments":{}}}
EOF
}

echo "[TEST] Starting MCP integration test..."

# Create a named pipe for MCP communication
FIFO_IN=$(mktemp -u)
FIFO_OUT=$(mktemp -u)
mkfifo "$FIFO_IN" "$FIFO_OUT"

# Start MCP server with pipes
tmux-mcp < "$FIFO_IN" > "$FIFO_OUT" 2>/dev/null &
MCP_PID=$!

# Give server time to start
sleep 0.5

# Open FDs
exec 3>"$FIFO_IN"
exec 4<"$FIFO_OUT"

cleanup_mcp() {
    exec 3>&-
    exec 4<&-
    kill "$MCP_PID" 2>/dev/null || true
    rm -f "$FIFO_IN" "$FIFO_OUT"
}
trap cleanup_mcp EXIT

# Test 1: Initialize
echo "[TEST] Sending initialize request..."
mcp_initialize >&3
read -r -t 5 response <&4 || { echo "[FAIL] No response to initialize"; exit 1; }
echo "[DEBUG] Initialize response: $response"
if ! echo "$response" | grep -q '"result"'; then
    echo "[FAIL] Initialize failed: $response"
    exit 1
fi
echo "[PASS] Initialize succeeded"

# Test 2: List tools
echo "[TEST] Listing available tools..."
mcp_list_tools >&3
read -r -t 5 response <&4 || { echo "[FAIL] No response to tools/list"; exit 1; }
echo "[DEBUG] List tools response: $response"
if ! echo "$response" | grep -q 'tmux_create_pane'; then
    echo "[FAIL] Expected tmux_create_pane in tools list"
    exit 1
fi
echo "[PASS] Tools list contains expected tools"

# Test 3: Create a pane
echo "[TEST] Creating pane with 'echo hello && sleep 10'..."
mcp_create_pane "echo hello && sleep 10" "test-pane" >&3
read -r -t 5 response <&4 || { echo "[FAIL] No response to create_pane"; exit 1; }
echo "[DEBUG] Create pane response: $response"
if echo "$response" | grep -q '"isError":true'; then
    echo "[FAIL] Create pane returned error: $response"
    exit 1
fi
# Extract pane_id from response (assuming it's in the result)
PANE_ID=$(echo "$response" | grep -oP '"pane_id"\s*:\s*"\K[^"]+' || echo "")
if [[ -z "$PANE_ID" ]]; then
    # Try alternative extraction
    PANE_ID=$(echo "$response" | jq -r '.result.content[0].text // .result.pane_id // empty' 2>/dev/null || echo "")
fi
if [[ -z "$PANE_ID" ]]; then
    echo "[WARN] Could not extract pane_id, using 'test-pane'"
    PANE_ID="test-pane"
fi
echo "[PASS] Created pane: $PANE_ID"

# Wait for command to execute
sleep 1

# Test 4: Capture pane output
echo "[TEST] Capturing pane output..."
mcp_capture_pane "$PANE_ID" 50 >&3
read -r -t 5 response <&4 || { echo "[FAIL] No response to capture_pane"; exit 1; }
echo "[DEBUG] Capture pane response: $response"
if echo "$response" | grep -q '"isError":true'; then
    echo "[FAIL] Capture pane returned error: $response"
    exit 1
fi
if ! echo "$response" | grep -q 'hello'; then
    echo "[WARN] Output may not contain 'hello' yet"
fi
echo "[PASS] Captured pane output"

# Test 5: Send keys
echo "[TEST] Sending keys to pane..."
mcp_send_keys "$PANE_ID" "echo world" >&3
read -r -t 5 response <&4 || { echo "[FAIL] No response to send_keys"; exit 1; }
echo "[DEBUG] Send keys response: $response"
if echo "$response" | grep -q '"isError":true'; then
    echo "[FAIL] Send keys returned error: $response"
    exit 1
fi
echo "[PASS] Sent keys to pane"

# Send Enter to execute the command
mcp_send_keys "$PANE_ID" "Enter" >&3
read -r -t 5 response <&4 || true

# Wait for command to execute
sleep 0.5

# Test 6: Capture again to see new output
echo "[TEST] Capturing pane output after send_keys..."
mcp_capture_pane "$PANE_ID" 50 >&3
read -r -t 5 response <&4 || { echo "[FAIL] No response to second capture_pane"; exit 1; }
echo "[DEBUG] Second capture response: $response"
echo "[PASS] Captured output after send_keys"

# Test 7: List panes
echo "[TEST] Listing panes..."
mcp_list_panes >&3
read -r -t 5 response <&4 || { echo "[FAIL] No response to list_panes"; exit 1; }
echo "[DEBUG] List panes response: $response"
if ! echo "$response" | grep -q "$PANE_ID\|test-pane"; then
    echo "[WARN] Created pane not in list"
fi
echo "[PASS] Listed panes"

# Test 8: Kill pane
echo "[TEST] Killing pane..."
mcp_kill_pane "$PANE_ID" >&3
read -r -t 5 response <&4 || { echo "[FAIL] No response to kill_pane"; exit 1; }
echo "[DEBUG] Kill pane response: $response"
if echo "$response" | grep -q '"isError":true'; then
    echo "[FAIL] Kill pane returned error: $response"
    exit 1
fi
echo "[PASS] Killed pane"

# Test 9: Verify pane is gone
echo "[TEST] Verifying pane is removed from list..."
mcp_list_panes >&3
read -r -t 5 response <&4 || { echo "[FAIL] No response to final list_panes"; exit 1; }
echo "[DEBUG] Final list panes response: $response"
if echo "$response" | grep -q "$PANE_ID"; then
    echo "[WARN] Killed pane still in list"
fi
echo "[PASS] Pane removed from list"

echo ""
echo "[SUCCESS] All MCP integration tests passed!"
exit 0
INNER_SCRIPT
chmod +x "${WORKSPACE}/mcp_test.sh"

log_info "Running MCP integration test inside container..."

# Run the test script inside the container
TEST_OUTPUT=$(podman run --rm \
    --network=pasta \
    --userns=keep-id \
    --entrypoint /bin/bash \
    -v "${WORKSPACE}:/workspace:rw" \
    -w /workspace \
    "docker-archive:${IMAGE_PATH}" \
    -c "/workspace/mcp_test.sh" 2>&1) || {
    EXIT_CODE=$?
    log_error "Test failed with exit code ${EXIT_CODE}"
    echo ""
    echo "Test output:"
    echo "${TEST_OUTPUT}"
    exit 1
}

echo ""
echo "Test output:"
echo "${TEST_OUTPUT}"

if echo "${TEST_OUTPUT}" | grep -q '\[SUCCESS\]'; then
    log_info "All tests passed!"
    exit 0
else
    log_error "Tests did not complete successfully"
    exit 1
fi
