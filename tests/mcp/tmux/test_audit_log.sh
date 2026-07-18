#!/usr/bin/env bash
# Test: Audit logging
#
# Tests:
# 1. Enable audit logging via TMUX_DEBUG_AUDIT env var
# 2. Perform operations (create, send_keys, capture, kill, list)
# 3. Verify log file created
# 4. Verify JSON Lines format
# 5. Verify log entries contain expected fields

set -euo pipefail

# shellcheck source=test_lib.sh
source "$(dirname "$0")/test_lib.sh"

main() {
    log_test "Starting audit_log tests..."

    # Create temp directory for audit log
    setup_temp_dir
    local audit_log="${TEMP_DIR}/audit.log"

    # Start MCP server with audit logging enabled
    log_test "Starting MCP server with audit logging..."
    start_mcp_server "TMUX_DEBUG_AUDIT=${audit_log}"

    # Initialize session
    log_test "Initializing MCP session..."
    local response
    response=$(mcp_initialize)
    assert_success "$response" "Initialize should succeed"
    mcp_initialized

    # Perform various operations
    log_test "Performing operations to generate audit log entries..."

    # Create pane
    response=$(mcp_create_pane "echo 'audit test' && sleep 60" "audit-pane")
    assert_success "$response" "Create pane should succeed"
    local pane_id
    pane_id=$(extract_pane_id "$response")
    log_pass "Created pane: $pane_id"

    # Send keys
    response=$(mcp_send_keys "$pane_id" "echo logged")
    assert_success "$response" "Send keys should succeed"
    log_pass "Sent keys"

    # Capture pane
    response=$(mcp_capture_pane "$pane_id" 50)
    assert_success "$response" "Capture should succeed"
    log_pass "Captured pane"

    # List panes
    response=$(mcp_list_panes)
    assert_success "$response" "List panes should succeed"
    log_pass "Listed panes"

    # Kill pane
    response=$(mcp_kill_pane "$pane_id")
    assert_success "$response" "Kill pane should succeed"
    log_pass "Killed pane"

    # Stop server to flush logs
    stop_mcp_server

    # Test: Verify log file exists
    log_test "Verifying audit log file exists..."
    if [[ ! -f "$audit_log" ]]; then
        log_fail "Audit log file not created"
        exit 1
    fi
    log_pass "Audit log file created: $audit_log"

    # Test: Verify file has content
    log_test "Verifying audit log has content..."
    local line_count
    line_count=$(wc -l < "$audit_log")
    if [[ "$line_count" -lt 4 ]]; then
        log_fail "Audit log should have at least 4 entries (create, send_keys, capture, kill)"
        log_error "Actual line count: $line_count"
        exit 1
    fi
    log_pass "Audit log has $line_count entries"

    # Test: Verify each line is valid JSON
    log_test "Verifying JSON Lines format..."
    local line_num=0
    # Use a subshell to avoid fd inheritance issues from test_lib.sh
    line_num=$(
        count=0
        while IFS= read -r line; do
            ((count++))
            if ! echo "$line" | jq . >/dev/null 2>&1; then
                echo "FAIL: Line $count is not valid JSON: $line" >&2
                exit 1
            fi
        done < "$audit_log"
        echo "$count"
    )
    if [[ -z "$line_num" ]]; then
        log_fail "Failed to parse audit log"
        exit 1
    fi
    log_pass "All $line_num lines are valid JSON"

    # Test: Verify create_pane entry
    log_test "Verifying create_pane log entry..."
    local create_entry
    create_entry=$(grep '"tool":"create_pane"' "$audit_log" || true)
    if [[ -z "$create_entry" ]]; then
        log_fail "No create_pane entry found in audit log"
        exit 1
    fi

    # Verify required fields
    local ts tool logged_pane_id command
    ts=$(echo "$create_entry" | jq -r '.ts')
    tool=$(echo "$create_entry" | jq -r '.tool')
    logged_pane_id=$(echo "$create_entry" | jq -r '.pane_id')
    command=$(echo "$create_entry" | jq -r '.command')

    assert_ne "null" "$ts" "Entry should have timestamp"
    assert_eq "create_pane" "$tool" "Tool should be create_pane"
    assert_eq "$pane_id" "$logged_pane_id" "Pane ID should match"
    assert_contains "$command" "audit test" "Command should be logged"
    log_pass "create_pane entry has correct fields"

    # Test: Verify send_keys entry
    log_test "Verifying send_keys log entry..."
    local send_entry
    send_entry=$(grep '"tool":"send_keys"' "$audit_log" || true)
    if [[ -z "$send_entry" ]]; then
        log_fail "No send_keys entry found in audit log"
        exit 1
    fi

    local keys
    keys=$(echo "$send_entry" | jq -r '.keys')
    assert_contains "$keys" "logged" "Keys should be logged"
    log_pass "send_keys entry has correct fields"

    # Test: Verify capture_pane entry
    log_test "Verifying capture_pane log entry..."
    local capture_entry
    capture_entry=$(grep '"tool":"capture_pane"' "$audit_log" || true)
    if [[ -z "$capture_entry" ]]; then
        log_fail "No capture_pane entry found in audit log"
        exit 1
    fi

    local lines output_bytes
    lines=$(echo "$capture_entry" | jq -r '.lines')
    output_bytes=$(echo "$capture_entry" | jq -r '.output_bytes')
    assert_eq "50" "$lines" "Lines count should match"
    # output_bytes should be a number >= 0
    if ! [[ "$output_bytes" =~ ^[0-9]+$ ]]; then
        log_fail "output_bytes should be a number"
        exit 1
    fi
    log_pass "capture_pane entry has correct fields"

    # Test: Verify kill_pane entry
    log_test "Verifying kill_pane log entry..."
    local kill_entry
    kill_entry=$(grep '"tool":"kill_pane"' "$audit_log" || true)
    if [[ -z "$kill_entry" ]]; then
        log_fail "No kill_pane entry found in audit log"
        exit 1
    fi
    log_pass "kill_pane entry present"

    # Test: Verify timestamp format (ISO 8601)
    log_test "Verifying timestamp format..."
    ts=$(echo "$create_entry" | jq -r '.ts')
    if [[ ! "$ts" =~ ^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]Z$ ]]; then
        log_fail "Timestamp is not ISO 8601 UTC: $ts"
        exit 1
    fi
    log_pass "Timestamp is ISO 8601 UTC: $ts"

    echo ""
    log_pass "All audit_log tests passed!"
    return 0
}

main "$@"
