#!/usr/bin/env bash
# Test: Exited pane handling
#
# Tests:
# 1. Create pane with short-lived command
# 2. Verify pane remains after process exits (remain-on-exit)
# 3. Verify status transitions to "exited"
# 4. Verify output can still be captured from exited pane

set -euo pipefail

# shellcheck source=test_lib.sh
source "$(dirname "$0")/test_lib.sh"

main() {
    log_test "Starting exited_pane tests..."

    # Start MCP server
    start_mcp_server

    # Initialize session
    log_test "Initializing MCP session..."
    local response
    response=$(mcp_initialize)
    assert_success "$response" "Initialize should succeed"
    mcp_initialized

    # Test 1: Create pane with a shell
    log_test "Test 1: Create pane with bash shell..."
    response=$(mcp_create_pane "bash" "exit-test")
    assert_success "$response" "Create pane should succeed"

    local pane_id
    pane_id=$(extract_pane_id "$response")
    assert_ne "" "$pane_id" "Pane ID should be extracted"
    log_pass "Created pane: $pane_id"

    sleep 0.5

    # Test 2: Verify initial status is "running"
    log_test "Test 2: Verify initial status is 'running'..."
    response=$(mcp_list_panes)
    assert_success "$response" "List panes should succeed"

    local content
    content=$(get_content_text "$response")

    local pane_data
    pane_data=$(echo "$content" | jq ".[] | select(.id == \"$pane_id\")" 2>/dev/null) || {
        log_fail "Could not parse pane list JSON"
        exit 1
    }

    local status
    status=$(echo "$pane_data" | jq -r '.status')
    assert_eq "running" "$status" "Initial pane status should be 'running'"
    log_pass "Initial status is 'running'"

    # Test 3: Send output to the pane before it exits
    log_test "Test 3: Generate some output..."
    response=$(mcp_send_keys "$pane_id" "echo 'goodbye world'")
    assert_success "$response"
    response=$(mcp_send_keys "$pane_id" "Enter")
    assert_success "$response"
    sleep 0.3
    log_pass "Sent output command"

    # Test 4: Capture output while running
    log_test "Test 4: Capture output while running..."
    response=$(mcp_capture_pane "$pane_id" 50)
    assert_success "$response" "Capture should succeed"

    content=$(get_content_text "$response")
    assert_contains "$content" "goodbye world" "Output should be captured"
    log_pass "Output captured while running"

    # Test 5: Exit the shell and verify pane remains (remain-on-exit works)
    log_test "Test 5: Exit shell and verify pane remains with 'exited' status..."
    response=$(mcp_send_keys "$pane_id" "exit")
    assert_success "$response"
    response=$(mcp_send_keys "$pane_id" "Enter")
    assert_success "$response"

    # Poll for pane status to become "exited" (tmux needs time to detect process exit)
    local attempts=0
    local max_attempts=10
    status="running"
    while [[ "$status" == "running" ]] && [[ "$attempts" -lt "$max_attempts" ]]; do
        sleep 0.5
        response=$(mcp_list_panes)
        assert_success "$response" "List panes should succeed"
        content=$(get_content_text "$response")

        pane_data=$(echo "$content" | jq ".[] | select(.id == \"$pane_id\")" 2>/dev/null) || {
            log_fail "Pane should still exist after process exits (remain-on-exit)"
            exit 1
        }

        status=$(echo "$pane_data" | jq -r '.status')
        attempts=$((attempts + 1))
    done

    assert_eq "exited" "$status" "Pane status should be 'exited' after process terminates"
    log_pass "Pane status is 'exited'"

    # Test 6: Capture output from exited pane (verify final output preserved)
    log_test "Test 6: Capture output from exited pane..."
    response=$(mcp_capture_pane "$pane_id" 50)
    assert_success "$response" "Capture from exited pane should succeed"

    content=$(get_content_text "$response")
    assert_contains "$content" "goodbye world" "Final output should be preserved in exited pane"
    log_pass "Output captured from exited pane"

    # Test 7: Kill the exited pane
    log_test "Test 7: Kill exited pane..."
    response=$(mcp_kill_pane "$pane_id")
    assert_success "$response" "Kill exited pane should succeed"

    response=$(mcp_list_panes)
    content=$(get_content_text "$response")
    assert_not_contains "$content" "$pane_id" "Pane should be removed after kill"
    log_pass "Exited pane killed successfully"

    # Test 8: Create pane with command that exits immediately
    log_test "Test 8: Create pane with immediately-exiting command..."
    response=$(mcp_create_pane "echo 'final message' && exit 0" "quick-exit")
    assert_success "$response" "Create pane should succeed"

    local pane_id2
    pane_id2=$(extract_pane_id "$response")
    assert_ne "" "$pane_id2" "Pane ID should be extracted"
    log_pass "Created quick-exit pane: $pane_id2"

    # Poll for pane status to become "exited"
    attempts=0
    status="running"
    while [[ "$status" == "running" ]] && [[ "$attempts" -lt "$max_attempts" ]]; do
        sleep 0.5
        response=$(mcp_list_panes)
        content=$(get_content_text "$response")
        pane_data=$(echo "$content" | jq ".[] | select(.id == \"$pane_id2\")" 2>/dev/null) || {
            log_fail "Quick-exit pane should still exist after process exits"
            exit 1
        }

        status=$(echo "$pane_data" | jq -r '.status')
        attempts=$((attempts + 1))
    done

    assert_eq "exited" "$status" "Quick-exit pane status should be 'exited'"

    # Verify output from quick-exit pane
    response=$(mcp_capture_pane "$pane_id2" 50)
    assert_success "$response"
    content=$(get_content_text "$response")
    assert_contains "$content" "final message" "Quick-exit pane should preserve output"
    log_pass "Quick-exit pane preserved output"

    # Cleanup
    log_test "Cleanup: killing remaining panes..."
    mcp_kill_pane "$pane_id2" >/dev/null

    echo ""
    log_pass "All exited_pane tests passed!"
    return 0
}

main "$@"
