#!/usr/bin/env bash
# E2E Test Runner for tmux-mcp sandbox tests
#
# This script runs all end-to-end tests for the tmux-mcp MCP server
# running inside wrix sandbox containers.
#
# Prerequisites:
# - nix (with flakes enabled)
# - podman
#
# Usage:
#   ./run-e2e.sh              # Run all tests
#   ./run-e2e.sh <test>       # Run specific test (e.g., sandbox_debug_profile)
#   ./run-e2e.sh --list       # List available tests
#   ./run-e2e.sh --help       # Show help
#
# Exit codes:
#   0 - All tests passed
#   1 - Some tests failed
#   2 - Prerequisites not met

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC2034  # REPO_ROOT used by individual test scripts
export REPO_ROOT="${SCRIPT_DIR}/../../.."

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# Test registry
declare -A TESTS=(
    ["sandbox_debug_profile"]="test_sandbox_debug_profile.sh"
    ["mcp_in_sandbox"]="test_mcp_in_sandbox.sh"
    ["profile_composition"]="test_profile_composition.sh"
    ["filesystem_isolation"]="test_filesystem_isolation.sh"
    ["mcp_audit_config"]="test_mcp_audit_config.sh"
)

# Test descriptions
declare -A TEST_DESCRIPTIONS=(
    ["sandbox_debug_profile"]="Build sandbox with MCP opt-in, verify tmux+MCP present"
    ["mcp_in_sandbox"]="Run MCP server inside sandbox, exercise all tools"
    ["profile_composition"]="Build rust + MCP opt-in, verify composition works"
    ["filesystem_isolation"]="Verify pane commands only access /workspace"
    ["mcp_audit_config"]="Verify MCP audit configuration is passed correctly"
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
tmux-mcp E2E Test Runner

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
        printf "  %-25s %s\n" "$test_name" "${TEST_DESCRIPTIONS[$test_name]}"
    done
    echo ""
    echo "Examples:"
    echo "  $(basename "$0")                      # Run all tests"
    echo "  $(basename "$0") sandbox_debug_profile  # Run specific test"
    echo "  $(basename "$0") --keep-going         # Run all tests, continue on failure"
}

list_tests() {
    echo "Available tests:"
    echo ""
    for test_name in "${!TESTS[@]}"; do
        printf "  %-25s %s\n" "$test_name" "${TEST_DESCRIPTIONS[$test_name]}"
    done | sort
}

check_prerequisites() {
    local failed=0

    if ! command -v nix &>/dev/null; then
        log_error "nix is required but not installed"
        log_info "Install from: https://nixos.org/download.html"
        failed=1
    fi

    if ! command -v podman &>/dev/null; then
        log_error "podman is required but not installed"
        log_info "Install with: nix develop (or your package manager)"
        failed=1
    fi

    # Check for pasta network support
    if command -v podman &>/dev/null; then
        if ! podman info 2>/dev/null | grep -q "pasta\|slirp4netns"; then
            log_warn "pasta or slirp4netns may be required for rootless networking"
        fi
    fi

    return $failed
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
    # shellcheck disable=SC2034  # verbose reserved for future use
    local verbose=0
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
                # shellcheck disable=SC2034
                verbose=1
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
        # Sort tests by dependency order
        tests_to_run=(
            "sandbox_debug_profile"    # Basic: does image build?
            "mcp_audit_config"          # Config: is audit param passed?
            "filesystem_isolation"      # Security: is sandbox isolated?
            "mcp_in_sandbox"            # Function: does MCP work?
            "profile_composition"       # Advanced: do profiles compose?
        )
    fi

    echo ""
    log_header "=========================================="
    log_header "  tmux-mcp E2E Test Suite"
    log_header "=========================================="
    echo ""

    log_info "Tests to run: ${tests_to_run[*]}"
    echo ""

    # Check prerequisites
    log_info "Checking prerequisites..."
    if ! check_prerequisites; then
        log_error "Prerequisites not met"
        exit 2
    fi
    log_info "Prerequisites OK"
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
            ((passed++))
        else
            ((failed++))
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
