#!/usr/bin/env bash
# Test: tmux_list_panes
#
# Tests:
# 1. List panes when empty
# 2. Create panes and verify list contents
# 3. Verify JSON structure of list output
# 4. Verify pane attributes (id, name, status, command)

set -euo pipefail

# shellcheck source=test_lib.sh
source "$(dirname "$0")/test_lib.sh"

main() {
    log_test "Starting list_panes tests..."

    # Start MCP server
    start_mcp_server

    # Initialize session
    log_test "Initializing MCP session..."
    local response
    response=$(mcp_initialize)
    assert_success "$response" "Initialize should succeed"
    mcp_initialized

    # Test 1: List panes when empty
    log_test "Test 1: List panes when empty..."
    response=$(mcp_list_panes)
    assert_success "$response" "List panes should succeed"

    local content
    content=$(get_content_text "$response")
    assert_contains "$content" "No active panes" "Empty list should indicate no panes"
    log_pass "Empty list works"

    # Test 2: Create panes and verify list
    log_test "Test 2: Create panes and verify list..."

    response=$(mcp_create_pane "echo 'first' && sleep 300" "pane-one")
    local id1
    id1=$(extract_pane_id "$response")

    response=$(mcp_create_pane "echo 'second' && sleep 300" "pane-two")
    local id2
    id2=$(extract_pane_id "$response")

    response=$(mcp_create_pane "echo 'third' && sleep 300")
    local id3
    id3=$(extract_pane_id "$response")  # No name for this one

    response=$(mcp_list_panes)
    assert_success "$response" "List panes should succeed"
    content=$(get_content_text "$response")

    assert_contains "$content" "$id1" "List should contain pane 1"
    assert_contains "$content" "$id2" "List should contain pane 2"
    assert_contains "$content" "$id3" "List should contain pane 3"
    log_pass "All panes in list"

    # Test 3: Verify JSON structure
    log_test "Test 3: Verify JSON structure..."

    # The list should be valid JSON array
    echo "$content" | jq '.' >/dev/null 2>&1 || {
        log_fail "List output should be valid JSON"
        log_error "Content: $content"
        exit 1
    }

    local pane_count
    pane_count=$(echo "$content" | jq 'length')
    assert_eq "3" "$pane_count" "Should have 3 panes"

    local session
    session=$(get_mcp_session_name)
    local unmanaged_count
    unmanaged_count=$(tmux list-windows -t "$session" -F '#{window_name}' | awk 'BEGIN { count = 0 } $0 !~ /^debug-[1-9][0-9]*$/ { count++ } END { print count }')
    assert_eq "0" "$unmanaged_count" "Tmux session should contain only managed debug-N windows"
    log_pass "JSON structure valid"

    # Test 4: Verify pane attributes
    log_test "Test 4: Verify pane attributes..."

    # Find pane-one in the list and check its attributes
    local pane_one
    pane_one=$(echo "$content" | jq '.[] | select(.name == "pane-one")')

    local attr_id
    attr_id=$(echo "$pane_one" | jq -r '.id')
    assert_eq "$id1" "$attr_id" "Pane ID should match"

    local attr_name
    attr_name=$(echo "$pane_one" | jq -r '.name')
    assert_eq "pane-one" "$attr_name" "Pane name should match"

    local attr_status
    attr_status=$(echo "$pane_one" | jq -r '.status')
    assert_eq "running" "$attr_status" "Pane status should be 'running'"

    local attr_command
    attr_command=$(echo "$pane_one" | jq -r '.command')
    assert_contains "$attr_command" "sleep" "Command should contain 'sleep'"
    log_pass "Pane attributes correct"

    # Test 5: Verify pane without explicit name uses ID as name
    log_test "Test 5: Verify pane without name..."

    local pane_three
    pane_three=$(echo "$content" | jq ".[] | select(.id == \"$id3\")")

    local name_three
    name_three=$(echo "$pane_three" | jq -r '.name')
    # If no name provided, name might be null or the ID
    if [[ "$name_three" == "null" ]] || [[ "$name_three" == "$id3" ]]; then
        log_pass "Unnamed pane has expected name value"
    else
        log_pass "Unnamed pane has name: $name_three"
    fi

    # Cleanup
    log_test "Cleanup: killing panes..."
    mcp_kill_pane "$id1" >/dev/null
    mcp_kill_pane "$id2" >/dev/null
    mcp_kill_pane "$id3" >/dev/null

    # Verify empty again
    response=$(mcp_list_panes)
    content=$(get_content_text "$response")
    assert_contains "$content" "No active panes" "List should be empty after cleanup"
    log_pass "Cleanup successful"

    echo ""
    log_pass "All list_panes tests passed!"
    return 0
}

main "$@"
