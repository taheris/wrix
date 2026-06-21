#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/mcp/playwright/lib.sh
source "${SCRIPT_DIR}/lib.sh"
playwright_require_linux

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

log_test() { echo -e "${BLUE}[TEST]${NC} $*"; }
log_pass() { echo -e "${GREEN}[PASS]${NC} $*"; }
log_fail() { echo -e "${RED}[FAIL]${NC} $*"; }
log_info() { echo -e "${GREEN}[INFO]${NC} $*"; }

TEMP_DIR=""

cleanup() {
    local exit_code=$?
    if [[ -n "${TEMP_DIR:-}" && -d "$TEMP_DIR" ]]; then
        rm -rf "$TEMP_DIR"
    fi
    exit "$exit_code"
}
trap cleanup EXIT

contains_line() {
    local needle="$1"
    local haystack="$2"

    grep -Fx "$needle" "$haystack" >/dev/null
}

test_image_contains_chromium() {
    log_test "test_image_contains_chromium: mcp.playwright sandbox image closes over chromium and server"

    TEMP_DIR=$(mktemp -d)
    local user_data_dir="${TEMP_DIR}/user-data"
    local config_file chrome_path browsers_path mcp_path package_names_json closure_path image_path image_closure

    config_file=$(playwright_config_path "$user_data_dir") || return 1
    chrome_path=$(jq -r '.browser.launchOptions.executablePath' "$config_file") || return 1
    browsers_path=$(playwright_build_package playwright-browsers) || return 1
    mcp_path=$(playwright_build_package playwright-mcp) || return 1

    log_info "Generated config: $config_file"
    log_info "Chromium path: $chrome_path"
    log_info "Browsers package: $browsers_path"
    log_info "MCP server package: $mcp_path"

    if [[ "$chrome_path" != "${browsers_path}"/*/chrome-linux64/chrome ]]; then
        log_fail "Generated chromium path is not derived from playwright-browsers"
        return 1
    fi

    if [[ ! -x "$chrome_path" ]]; then
        log_fail "Chromium binary is not executable: $chrome_path"
        return 1
    fi

    if [[ ! -x "${mcp_path}/bin/playwright-mcp" ]]; then
        log_fail "playwright-mcp binary is not executable: ${mcp_path}/bin/playwright-mcp"
        return 1
    fi

    package_names_json=$(playwright_sandbox_package_names_json --argstr userDataDir "$user_data_dir") || return 1
    if ! jq -e 'index("playwright-mcp") and index("playwright-browsers")' <<<"$package_names_json" >/dev/null; then
        log_fail "mcp.playwright sandbox profile did not include the Playwright packages"
        echo "$package_names_json" >&2
        return 1
    fi
    log_info "mcp.playwright sandbox profile includes Playwright packages"

    closure_path=$(playwright_sandbox_package_closure --argstr userDataDir "$user_data_dir") || return 1
    if ! contains_line "$browsers_path" "${closure_path}/store-paths"; then
        log_fail "Sandbox package closure is missing playwright-browsers"
        return 1
    fi
    if ! contains_line "$mcp_path" "${closure_path}/store-paths"; then
        log_fail "Sandbox package closure is missing playwright-mcp"
        return 1
    fi
    log_info "Sandbox package closure contains Playwright packages"

    image_path=$(playwright_sandbox_image --argstr userDataDir "$user_data_dir") || return 1
    image_closure=$(nix path-info --recursive "$image_path") || return 1
    if [[ "$image_closure" != *"$browsers_path"* ]]; then
        log_fail "Built sandbox image closure is missing playwright-browsers"
        return 1
    fi
    if [[ "$image_closure" != *"$mcp_path"* ]]; then
        log_fail "Built sandbox image closure is missing playwright-mcp"
        return 1
    fi
    log_info "Built sandbox image closure contains Playwright packages"

    log_pass "test_image_contains_chromium"
}

main() {
    echo ""
    log_info "=========================================="
    log_info "  playwright-mcp Build Tests"
    log_info "=========================================="
    echo ""

    local passed=0
    local failed=0

    if test_image_contains_chromium; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
    fi
    echo ""

    echo "=========================================="
    log_info "Results: $passed passed, $failed failed"
    echo "=========================================="

    if ((failed > 0)); then
        exit 1
    fi
}

main "$@"
