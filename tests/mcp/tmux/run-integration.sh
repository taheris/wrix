#!/usr/bin/env bash
# Integration Test Runner for tmux-mcp
#
# This script runs all integration tests for the tmux-mcp MCP server.
# Tests exercise the MCP server directly (not inside containers) and require tmux.
#
# Prerequisites (provided by the NixOS VM in tests/mcp/tmux/check.nix):
# - tmux
# - tmux-mcp binary
# - jq (for JSON parsing)
#
# Usage:
#   ./run-integration.sh              # Run all tests
#   ./run-integration.sh <test>       # Run specific test (e.g., create_pane)
#   ./run-integration.sh --list       # List available tests
#   ./run-integration.sh --help       # Show help
#
# Exit codes:
#   0 - All tests passed
#   1 - Some tests failed

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export REPO_ROOT="${SCRIPT_DIR}/../.."

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# Test registry
declare -A TESTS=(
    ["create_pane"]="test_create_pane.sh"
    ["send_keys"]="test_send_keys.sh"
    ["capture_pane"]="test_capture_pane.sh"
    ["kill_pane"]="test_kill_pane.sh"
    ["list_panes"]="test_list_panes.sh"
    ["exited_pane"]="test_exited_pane.sh"
    ["error_handling"]="test_error_handling.sh"
    ["audit_log"]="test_audit_log.sh"
    ["audit_full_capture"]="test_audit_full_capture.sh"
    ["cleanup_on_exit"]="test_cleanup_on_exit.sh"
)

# Test descriptions
declare -A TEST_DESCRIPTIONS=(
    ["create_pane"]="Create pane, verify tmux window exists, verify returned pane ID"
    ["send_keys"]="Create pane with shell, send echo hello, capture output, verify"
    ["capture_pane"]="Create pane running seq, capture with various line counts"
    ["kill_pane"]="Create pane, kill it, verify tmux window gone"
    ["list_panes"]="Create multiple panes, verify list returns all"
    ["exited_pane"]="Create exiting pane, verify status=exited, capture final output"
    ["error_handling"]="Send keys to nonexistent pane, verify isError response"
    ["audit_log"]="Enable audit logging, run operations, verify JSON Lines format"
    ["audit_full_capture"]="Enable full capture audit logging, capture output, verify file content"
    ["cleanup_on_exit"]="Start MCP server, create panes, kill server, verify cleanup"
)

log_info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*"
}

log_header() {
    echo -e "${BLUE}${BOLD}$*${NC}"
}

show_help() {
    cat << EOF
tmux-mcp Integration Test Runner

Usage:
  $(basename "$0") [OPTIONS] [TEST_NAME...]

Options:
  --help, -h      Show this help message
  --list, -l      List available tests
  --verbose, -v   Show verbose output
  --keep-going    Continue running tests after failure

Test Names:
EOF
    for test_name in "${!TESTS[@]}"; do
        printf "  %-20s %s\n" "$test_name" "${TEST_DESCRIPTIONS[$test_name]}"
    done | sort
    echo ""
    echo "Examples:"
    echo "  $(basename "$0")                      # Run all tests"
    echo "  $(basename "$0") create_pane          # Run specific test"
    echo "  $(basename "$0") --keep-going         # Run all tests, continue on failure"
}

list_tests() {
    echo "Available tests:"
    echo ""
    for test_name in "${!TESTS[@]}"; do
        printf "  %-20s %s\n" "$test_name" "${TEST_DESCRIPTIONS[$test_name]}"
    done | sort
}

run_test() {
    local test_name="$1"
    local test_file="${TESTS[$test_name]}"
    local test_path="${SCRIPT_DIR}/${test_file}"

    if [[ ! -f "$test_path" ]]; then
        log_error "Test file not found: $test_path"
        return 1
    fi

    if [[ ! -x "$test_path" ]]; then
        log_error "Test file not executable: $test_path"
        return 1
    fi

    log_header "Running: $test_name"
    log_info "${TEST_DESCRIPTIONS[$test_name]}"
    echo ""

    local start_time
    start_time=$(date +%s)

    if "$test_path"; then
        local end_time
        end_time=$(date +%s)
        local duration=$((end_time - start_time))
        echo ""
        log_info "PASSED: $test_name (${duration}s)"
        return 0
    else
        local exit_code=$?
        local end_time
        end_time=$(date +%s)
        local duration=$((end_time - start_time))
        echo ""
        log_error "FAILED: $test_name (${duration}s, exit code: $exit_code)"
        return 1
    fi
}

main() {
    local keep_going=0
    local tests_to_run=()

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --help|-h)
                show_help
                exit 0
                ;;
            --list|-l)
                list_tests
                exit 0
                ;;
            --verbose|-v)
                export VERBOSE=1
                shift
                ;;
            --keep-going)
                keep_going=1
                shift
                ;;
            -*)
                log_error "Unknown option: $1"
                show_help
                exit 1
                ;;
            *)
                if [[ -n "${TESTS[$1]:-}" ]]; then
                    tests_to_run+=("$1")
                else
                    log_error "Unknown test: $1"
                    echo ""
                    list_tests
                    exit 1
                fi
                shift
                ;;
        esac
    done

    # Default to all tests if none specified
    if [[ ${#tests_to_run[@]} -eq 0 ]]; then
        # Run tests in logical order
        tests_to_run=(
            "create_pane"       # Basic: can we create panes?
            "send_keys"         # Basic: can we send input?
            "capture_pane"      # Basic: can we capture output?
            "kill_pane"         # Basic: can we clean up?
            "list_panes"        # List functionality
            "exited_pane"       # Lifecycle: exited pane handling
            "error_handling"    # Errors: proper error responses
            "audit_log"         # Feature: audit logging
            "audit_full_capture" # Feature: full capture audit logging
            "cleanup_on_exit"   # Cleanup: session cleanup on exit
        )
    fi

    echo ""
    log_header "=========================================="
    log_header "  tmux-mcp Integration Test Suite"
    log_header "=========================================="
    echo ""

    log_info "Tests to run: ${tests_to_run[*]}"
    echo ""

    # Run tests
    local passed=0
    local failed=0
    local skipped=0
    local failed_tests=()

    for test_name in "${tests_to_run[@]}"; do
        echo ""
        echo "----------------------------------------"

        if run_test "$test_name"; then
            ((passed++)) || true # best-effort: arithmetic returns 1 when the prior value is zero.
        else
            ((failed++)) || true # best-effort: arithmetic returns 1 when the prior value is zero.
            failed_tests+=("$test_name")
            if [[ $keep_going -eq 0 ]]; then
                log_error "Stopping on first failure (use --keep-going to continue)"
                break
            fi
        fi
    done

    # Summary
    echo ""
    echo "=========================================="
    log_header "Test Summary"
    echo "=========================================="
    echo ""
    log_info "Passed:  $passed"
    if [[ $failed -gt 0 ]]; then
        log_error "Failed:  $failed"
        log_error "Failed tests: ${failed_tests[*]}"
    else
        log_info "Failed:  $failed"
    fi
    if [[ $skipped -gt 0 ]]; then
        log_warn "Skipped: $skipped"
    fi
    echo ""

    if [[ $failed -eq 0 ]]; then
        log_info "ALL TESTS PASSED"
        exit 0
    else
        log_error "SOME TESTS FAILED"
        exit 1
    fi
}

main "$@"
