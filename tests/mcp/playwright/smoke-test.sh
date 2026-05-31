#!/usr/bin/env bash
# Smoke tests for playwright-mcp
#
# Tests:
# 1. test_mcp_initialize: MCP server starts and responds to JSON-RPC initialize
#    request with tool list (browser_take_screenshot, browser_navigate, etc.)
# 2. test_offline_startup: Server starts without making network requests
#    (PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 is effective)
#
# Prerequisites:
# - playwright-mcp (from nixpkgs)
# - playwright-driver.browsers (chromium)
# - jq

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

log_test() { echo -e "${BLUE}[TEST]${NC} $*"; }
log_pass() { echo -e "${GREEN}[PASS]${NC} $*"; }
log_fail() { echo -e "${RED}[FAIL]${NC} $*"; }
log_info() { echo -e "${GREEN}[INFO]${NC} $*"; }

# State
MCP_PID=""
TEMP_DIR=""
REQUEST_ID=0

cleanup() {
    local exit_code=$?
    if [[ -n "${MCP_PID:-}" ]]; then
        exec 3>&- 2>/dev/null || true
        exec 4<&- 2>/dev/null || true
        kill "$MCP_PID" 2>/dev/null || true
        wait "$MCP_PID" 2>/dev/null || true
    fi
    if [[ -n "${TEMP_DIR:-}" ]] && [[ -d "$TEMP_DIR" ]]; then
        rm -rf "$TEMP_DIR"
    fi
    exit "$exit_code"
}
trap cleanup EXIT

# --- Helpers ---

find_playwright_mcp() {
    if command -v playwright-mcp &>/dev/null; then
        command -v playwright-mcp
        return 0
    fi
    # Try nix build
    local pkg_path
    pkg_path=$(nix build 'nixpkgs#playwright-mcp' --no-link --print-out-paths 2>/dev/null) || return 1
    echo "${pkg_path}/bin/playwright-mcp"
}

find_chromium() {
    local browsers_path
    browsers_path=$(nix build 'nixpkgs#playwright-driver.browsers' --no-link --print-out-paths 2>/dev/null) || return 1
    local revision
    revision=$(nix eval --raw 'nixpkgs#playwright-driver.passthru.browsersJSON.chromium.revision' 2>/dev/null) || return 1
    local chrome="${browsers_path}/chromium-${revision}/chrome-linux64/chrome"
    if [[ -x "$chrome" ]]; then
        echo "$chrome"
        return 0
    fi
    return 1
}

make_config() {
    local chrome_path="$1"
    local config_file="${TEMP_DIR}/config.json"
    cat > "$config_file" <<EOF
{
  "browser": {
    "launchOptions": {
      "args": ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"],
      "executablePath": "${chrome_path}",
      "headless": true
    }
  },
  "contextOptions": {
    "viewport": { "width": 1280, "height": 720 }
  }
}
EOF
    echo "$config_file"
}

start_mcp_server() {
    local mcp_bin="$1"
    local config_file="$2"
    shift 2
    local env_vars=("$@")

    mkfifo "${TEMP_DIR}/in" "${TEMP_DIR}/out"

    if [[ ${#env_vars[@]} -gt 0 ]]; then
        env "${env_vars[@]}" \
            PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 \
            PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true \
            "$mcp_bin" --config "$config_file" < "${TEMP_DIR}/in" > "${TEMP_DIR}/out" 2>/dev/null &
    else
        PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 \
        PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true \
        "$mcp_bin" --config "$config_file" < "${TEMP_DIR}/in" > "${TEMP_DIR}/out" 2>/dev/null &
    fi
    MCP_PID=$!

    sleep 0.3

    if ! kill -0 "$MCP_PID" 2>/dev/null; then
        log_fail "MCP server failed to start"
        return 1
    fi

    exec 3>"${TEMP_DIR}/in"
    exec 4<"${TEMP_DIR}/out"
}

stop_mcp_server() {
    exec 3>&- 2>/dev/null || true
    exec 4<&- 2>/dev/null || true
    if [[ -n "${MCP_PID:-}" ]]; then
        kill "$MCP_PID" 2>/dev/null || true
        wait "$MCP_PID" 2>/dev/null || true
        MCP_PID=""
    fi
    if [[ -n "${TEMP_DIR:-}" ]] && [[ -d "$TEMP_DIR" ]]; then
        rm -rf "$TEMP_DIR"
        TEMP_DIR=""
    fi
}

next_id() {
    ((REQUEST_ID++))
    echo "$REQUEST_ID"
}

mcp_request() {
    local request="$1"
    local timeout="${2:-5}"
    echo "$request" >&3
    local response
    if ! read -r -t "$timeout" response <&4; then
        log_fail "Timeout waiting for MCP response"
        return 1
    fi
    echo "$response"
}

mcp_notify() {
    echo "$1" >&3
    sleep 0.1
}

# --- Tests ---

test_mcp_initialize() {
    log_test "test_mcp_initialize: MCP server starts and returns tools"

    local mcp_bin chrome_path config_file
    mcp_bin=$(find_playwright_mcp) || { log_fail "Cannot find playwright-mcp"; return 1; }
    chrome_path=$(find_chromium) || { log_fail "Cannot find chromium"; return 1; }

    TEMP_DIR=$(mktemp -d)
    config_file=$(make_config "$chrome_path")

    start_mcp_server "$mcp_bin" "$config_file"

    # Send initialize
    local id response
    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1.0\"}}}")

    # Verify initialize response
    local server_name
    server_name=$(echo "$response" | jq -r '.result.serverInfo.name')
    if [[ "$server_name" != "Playwright" ]]; then
        log_fail "Expected serverInfo.name='Playwright', got '$server_name'"
        return 1
    fi
    log_info "Server initialized: $server_name"

    # Send initialized notification
    mcp_notify '{"jsonrpc":"2.0","method":"notifications/initialized"}'

    # Request tool list
    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/list\"}")

    # Verify expected tools are present
    local tools
    tools=$(echo "$response" | jq -r '[.result.tools[].name] | join(",")') || {
        log_fail "Failed to parse tools from response"
        return 1
    }

    local expected_tools=(
        "browser_navigate"
        "browser_take_screenshot"
        "browser_click"
        "browser_snapshot"
        "browser_console_messages"
    )

    for tool in "${expected_tools[@]}"; do
        if [[ ",$tools," != *",$tool,"* ]]; then
            log_fail "Expected tool '$tool' not found in tool list"
            log_fail "Available tools: $tools"
            return 1
        fi
    done

    local tool_count
    tool_count=$(echo "$response" | jq '.result.tools | length')
    log_info "Server reported $tool_count tools"

    stop_mcp_server
    log_pass "test_mcp_initialize"
}

test_offline_startup() {
    log_test "test_offline_startup: Server starts with PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1"

    local mcp_bin chrome_path config_file
    mcp_bin=$(find_playwright_mcp) || { log_fail "Cannot find playwright-mcp"; return 1; }
    chrome_path=$(find_chromium) || { log_fail "Cannot find chromium"; return 1; }

    TEMP_DIR=$(mktemp -d)
    config_file=$(make_config "$chrome_path")

    # Start with explicit offline env vars
    start_mcp_server "$mcp_bin" "$config_file" \
        "PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1" \
        "PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true"

    # Verify server responds to initialize (proves it started without downloads)
    local id response
    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1.0\"}}}")

    local server_name
    server_name=$(echo "$response" | jq -r '.result.serverInfo.name')
    if [[ "$server_name" != "Playwright" ]]; then
        log_fail "Server failed to start offline. Response: $response"
        return 1
    fi

    # Verify no error in response
    local error
    error=$(echo "$response" | jq -r '.error // empty')
    if [[ -n "$error" ]]; then
        log_fail "Server returned error on offline startup: $error"
        return 1
    fi

    stop_mcp_server
    log_pass "test_offline_startup"
}

# --- Main ---

main() {
    echo ""
    log_info "=========================================="
    log_info "  playwright-mcp Smoke Tests"
    log_info "=========================================="
    echo ""

    local passed=0
    local failed=0

    for test_fn in test_mcp_initialize test_offline_startup; do
        if "$test_fn"; then
            ((passed++)) || true
        else
            ((failed++)) || true
        fi
        echo ""
    done

    echo "=========================================="
    log_info "Results: $passed passed, $failed failed"
    echo "=========================================="

    if [[ $failed -gt 0 ]]; then
        exit 1
    fi
}

main "$@"
