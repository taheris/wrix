#!/usr/bin/env bash
# Test: Error handling
#
# Tests:
# 1. Send keys to nonexistent pane
# 2. Capture from nonexistent pane
# 3. Kill nonexistent pane
# 4. Create pane with missing command parameter
# 5. Send keys with missing parameters
# 6. Call unknown tool

set -euo pipefail

# shellcheck source=test_lib.sh
source "$(dirname "$0")/test_lib.sh"

main() {
    log_test "Starting error_handling tests..."

    # Start MCP server
    start_mcp_server

    # Initialize session
    log_test "Initializing MCP session..."
    local response
    response=$(mcp_initialize)
    assert_success "$response" "Initialize should succeed"
    mcp_initialized

    # Test 1: Send keys to nonexistent pane
    log_test "Test 1: Send keys to nonexistent pane..."
    response=$(mcp_send_keys "debug-999" "echo hello")
    assert_error "$response" "Send keys to nonexistent pane should fail"

    local content
    content=$(get_content_text "$response")
    assert_contains "$content" "not found" "Error should mention pane not found"
    log_pass "Send keys to nonexistent pane returns error"

    # Test 2: Capture from nonexistent pane
    log_test "Test 2: Capture from nonexistent pane..."
    response=$(mcp_capture_pane "debug-999" 50)
    assert_error "$response" "Capture from nonexistent pane should fail"

    content=$(get_content_text "$response")
    assert_contains "$content" "not found" "Error should mention pane not found"
    log_pass "Capture from nonexistent pane returns error"

    # Test 3: Kill nonexistent pane
    log_test "Test 3: Kill nonexistent pane..."
    response=$(mcp_kill_pane "debug-999")
    assert_error "$response" "Kill nonexistent pane should fail"

    content=$(get_content_text "$response")
    assert_contains "$content" "not found" "Error should mention pane not found"
    log_pass "Kill nonexistent pane returns error"

    # Test 4: Create pane with missing command parameter
    log_test "Test 4: Create pane with missing command..."
    local id
    id=$(next_request_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"tmux_create_pane\",\"arguments\":{}}}")
    assert_error "$response" "Create pane without command should fail"

    content=$(get_content_text "$response")
    assert_contains "$content" "command" "Error should mention missing command"
    log_pass "Missing command parameter returns error"

    # Test 5: Send keys with missing pane_id
    log_test "Test 5: Send keys with missing pane_id..."
    id=$(next_request_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"tmux_send_keys\",\"arguments\":{\"keys\":\"echo hello\"}}}")
    assert_error "$response" "Send keys without pane_id should fail"

    content=$(get_content_text "$response")
    assert_contains "$content" "pane_id" "Error should mention missing pane_id"
    log_pass "Missing pane_id parameter returns error"

    # Test 6: Send keys with missing keys parameter
    log_test "Test 6: Send keys with missing keys parameter..."
    # First create a valid pane
    response=$(mcp_create_pane "sleep 60" "test-pane")
    local pane_id
    pane_id=$(extract_pane_id "$response")

    id=$(next_request_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"tmux_send_keys\",\"arguments\":{\"pane_id\":\"$pane_id\"}}}")
    assert_error "$response" "Send keys without keys parameter should fail"

    content=$(get_content_text "$response")
    assert_contains "$content" "keys" "Error should mention missing keys"
    log_pass "Missing keys parameter returns error"

    # Test 7: Call unknown tool
    log_test "Test 7: Call unknown tool..."
    id=$(next_request_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"unknown_tool\",\"arguments\":{}}}")
    assert_error "$response" "Unknown tool should fail"

    content=$(get_content_text "$response")
    assert_contains "$content" "Unknown tool" "Error should mention unknown tool"
    log_pass "Unknown tool returns error"

    # Test 8: Verify errors contain helpful hints
    log_test "Test 8: Error messages contain helpful hints..."
    response=$(mcp_send_keys "debug-998" "test")
    content=$(get_content_text "$response")
    assert_contains "$content" "tmux_list_panes" "Error should suggest using list_panes"
    log_pass "Error messages include helpful hints"

    # Cleanup
    log_test "Cleanup: killing test pane..."
    mcp_kill_pane "$pane_id" >/dev/null

    echo ""
    log_pass "All error_handling tests passed!"
    return 0
}

main "$@"
