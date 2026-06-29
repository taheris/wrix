#!/usr/bin/env bash
# Test: Audit full capture files

set -euo pipefail

# shellcheck source=test_lib.sh
source "$(dirname "$0")/test_lib.sh"

main() {
    log_test "Starting audit_full_capture tests..."

    setup_temp_dir
    local audit_log="${TEMP_DIR}/audit.log"
    local capture_dir="${TEMP_DIR}/captures"

    log_test "Starting MCP server with full capture audit logging..."
    start_mcp_server "TMUX_DEBUG_AUDIT=${audit_log}" "TMUX_DEBUG_AUDIT_FULL=${capture_dir}"

    log_test "Initializing MCP session..."
    local response
    response=$(mcp_initialize)
    assert_success "$response" "Initialize should succeed"
    mcp_initialized

    log_test "Creating pane with capture content..."
    response=$(mcp_create_pane "echo audit-full-alpha && echo audit-full-beta && sleep 60" "audit-full")
    assert_success "$response" "Create pane should succeed"

    local pane_id
    pane_id=$(extract_pane_id "$response")
    assert_ne "" "$pane_id" "Pane ID should be extracted"

    sleep 0.5

    log_test "Capturing pane output..."
    response=$(mcp_capture_pane "$pane_id" 20)
    assert_success "$response" "Capture should succeed"

    local content
    content=$(get_content_text "$response")
    assert_contains "$content" "audit-full-alpha" "Capture should include first line"
    assert_contains "$content" "audit-full-beta" "Capture should include second line"

    stop_mcp_server

    log_test "Verifying full capture file..."
    if [[ ! -d "$capture_dir" ]]; then
        log_fail "Full capture directory was not created"
        exit 1
    fi

    local capture_file_count
    capture_file_count=$(find "$capture_dir" -maxdepth 1 -type f -name "${pane_id}-capture-*.txt" -print | wc -l)
    capture_file_count="${capture_file_count//[[:space:]]/}"
    assert_eq "1" "$capture_file_count" "Exactly one full capture file should be written"

    local capture_file
    capture_file=$(find "$capture_dir" -maxdepth 1 -type f -name "${pane_id}-capture-*.txt" -print | sort | head -n 1)
    if [[ -z "$capture_file" ]]; then
        log_fail "Full capture file was not found"
        exit 1
    fi

    local full_content
    full_content=$(<"$capture_file")
    assert_eq "$content" "$full_content" "Full capture file should match returned capture content"

    echo ""
    log_pass "All audit_full_capture tests passed!"
    return 0
}

main "$@"
