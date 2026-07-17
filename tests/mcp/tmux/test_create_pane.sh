#!/usr/bin/env bash
# Test: tmux_create_pane
#
# Tests:
# 1. Create a pane with command only
# 2. Create a pane with command and name
# 3. Verify tmux window is created
# 4. Verify pane ID is returned
# 5. Verify pane appears in list_panes

set -euo pipefail

# shellcheck source=test_lib.sh
source "$(dirname "$0")/test_lib.sh"

main() {
    log_test "Starting create_pane tests..."

    # Start MCP server
    start_mcp_server

    # Initialize session
    log_test "Initializing MCP session..."
    local response
    response=$(mcp_initialize)
    assert_success "$response" "Initialize should succeed"
    mcp_initialized

    # Test 1: Create pane with command only
    log_test "Test 1: Create pane with command only..."
    response=$(mcp_create_pane "sleep 60")
    assert_success "$response" "Create pane should succeed"

    local pane_id
    pane_id=$(extract_pane_id "$response")
    assert_ne "" "$pane_id" "Pane ID should be extracted"
    assert_contains "$pane_id" "debug-" "Pane ID should start with 'debug-'"
    log_pass "Created pane: $pane_id"

    # Test 2: Verify tmux window exists
    log_test "Test 2: Verify tmux window exists..."
    local session_name
    session_name=$(get_mcp_session_name)
    assert_tmux_session_exists "$session_name" "MCP tmux session should exist"

    # The window is named with the pane_id
    if ! mcp_tmux "$session_name" list-windows -t "$session_name" 2>/dev/null | grep -q .; then
        log_fail "Expected at least one window in session $session_name"
        exit 1
    fi
    log_pass "Tmux session and window exist"

    # Test 3: Create pane with command and name
    log_test "Test 3: Create pane with command and name..."
    response=$(mcp_create_pane "echo 'named pane' && sleep 60" "my-test-pane")
    assert_success "$response" "Create named pane should succeed"

    local pane_id2
    pane_id2=$(extract_pane_id "$response")
    assert_ne "" "$pane_id2" "Second pane ID should be extracted"
    assert_ne "$pane_id" "$pane_id2" "Pane IDs should be unique"
    log_pass "Created named pane: $pane_id2"

    # Test 4: Verify panes appear in list_panes
    log_test "Test 4: Verify panes appear in list_panes..."
    response=$(mcp_list_panes)
    assert_success "$response" "List panes should succeed"

    local content
    content=$(get_content_text "$response")
    assert_contains "$content" "$pane_id" "List should contain first pane ID"
    assert_contains "$content" "$pane_id2" "List should contain second pane ID"
    assert_contains "$content" "my-test-pane" "List should contain pane name"
    log_pass "Both panes appear in list"

    # Test 5: Verify command is stored
    log_test "Test 5: Verify command is stored..."
    assert_contains "$content" "sleep 60" "List should contain command"
    log_pass "Command stored correctly"

    # Cleanup: kill panes
    log_test "Cleanup: killing panes..."
    mcp_kill_pane "$pane_id" >/dev/null
    mcp_kill_pane "$pane_id2" >/dev/null

    echo ""
    log_pass "All create_pane tests passed!"
    return 0
}

main "$@"
