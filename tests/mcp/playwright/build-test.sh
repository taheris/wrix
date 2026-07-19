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
    local config_file chrome_path chrome_target chrome_helper_path browsers_path mcp_path package_names_json closure_path image_path image_closure

    config_file=$(playwright_config_path "$user_data_dir") || return 1
    chrome_path=$(jq -r '.browser.launchOptions.executablePath' "$config_file") || return 1
    chrome_target=$(playwright_chromium_executable_target "$chrome_path") || return 1
    chrome_helper_path="${chrome_path%/bin/chrome}"
    browsers_path=$(playwright_build_package playwright-browsers) || return 1
    mcp_path=$(playwright_build_package playwright-mcp) || return 1

    log_info "Generated config: $config_file"
    log_info "Chromium path: $chrome_path"
    log_info "Chromium target: $chrome_target"
    log_info "Browsers package: $browsers_path"
    log_info "MCP server package: $mcp_path"

    if ! playwright_chromium_path_is_derived_from_browsers "$chrome_path" "$browsers_path"; then
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
    if ! jq -e 'index("playwright-mcp") and index("playwright-browsers") and index("playwright-chromium-executable")' <<<"$package_names_json" >/dev/null; then
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
    if ! contains_line "$chrome_helper_path" "${closure_path}/store-paths"; then
        log_fail "Sandbox package closure is missing playwright-chromium-executable"
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
    if [[ "$image_closure" != *"$chrome_helper_path"* ]]; then
        log_fail "Built sandbox image closure is missing playwright-chromium-executable"
        return 1
    fi
    log_info "Built sandbox image closure contains Playwright packages"

    log_pass "test_image_contains_chromium"
}

test_image_derivation_closes_over_chromium() {
    log_test "test_image_derivation_closes_over_chromium: image derivation references chromium and server closures"

    local browsers_drv mcp_drv chrome_drv image_drv requisites
    browsers_drv=$(playwright_instantiate_mode package --argstr packageName playwright-browsers) || return 1
    mcp_drv=$(playwright_instantiate_mode package --argstr packageName playwright-mcp) || return 1
    chrome_drv=$(playwright_instantiate_mode package --argstr packageName playwright-chromium-executable) || return 1
    image_drv=$(playwright_instantiate_mode sandbox-image) || return 1
    requisites=$(nix-store -q --requisites --include-outputs "$image_drv") || return 1

    for required in "$browsers_drv" "$mcp_drv" "$chrome_drv"; do
        if ! grep -Fx "$required" <<<"$requisites" >/dev/null; then
            log_fail "Image derivation closure is missing $required"
            return 1
        fi
    done

    log_pass "test_image_derivation_closes_over_chromium"
}

main() {
    echo ""
    log_info "=========================================="
    log_info "  playwright-mcp Build Tests"
    log_info "=========================================="
    echo ""

    local passed=0 failed=0 test_fn
    local tests=(test_image_contains_chromium)
    if (($# > 0)); then
        tests=("$@")
    fi

    for test_fn in "${tests[@]}"; do
        if ! declare -F "$test_fn" >/dev/null; then
            log_fail "Unknown test function: $test_fn"
            failed=$((failed + 1))
        elif "$test_fn"; then
            passed=$((passed + 1))
        else
            failed=$((failed + 1))
        fi
        echo ""
    done

    echo "=========================================="
    log_info "Results: $passed passed, $failed failed"
    echo "=========================================="

    if ((failed > 0)); then
        exit 1
    fi
}

main "$@"
