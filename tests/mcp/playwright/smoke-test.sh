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

MCP_PID=""
TEMP_DIR=""
REQUEST_ID=0

cleanup() {
    local exit_code=$?
    stop_mcp_server
    remove_temp_dir
    exit "$exit_code"
}
trap cleanup EXIT

remove_temp_dir() {
    if [[ -n "${TEMP_DIR:-}" && -d "$TEMP_DIR" ]]; then
        rm -rf "$TEMP_DIR"
        TEMP_DIR=""
    fi
}

new_temp_dir() {
    remove_temp_dir
    TEMP_DIR=$(mktemp -d)
}

stop_mcp_server() {
    exec 3>&- 2>/dev/null || true # best-effort: fd 3 may not be open.
    exec 4<&- 2>/dev/null || true # best-effort: fd 4 may not be open.
    if [[ -n "${MCP_PID:-}" ]]; then
        kill "$MCP_PID" 2>/dev/null || true # best-effort: process may have exited.
        wait "$MCP_PID" 2>/dev/null || true # best-effort: process may already be reaped.
        MCP_PID=""
    fi
}

start_mcp_server() {
    local user_data_dir="$1"
    local headless="${2:-true}"
    local width="${3:-1280}"
    local height="${4:-720}"
    local config_json="${5:-{}}"
    local mcp_bin
    local server_args_output
    local server_env_output
    local server_args
    local server_env

    mkdir -p "$user_data_dir"
    mcp_bin=$(playwright_find_mcp) || return 1
    server_args_output=$(playwright_server_args "$user_data_dir" "$headless" "$width" "$height" "$config_json") || return 1
    server_env_output=$(playwright_server_env "$user_data_dir" "$headless" "$width" "$height" "$config_json") || return 1
    mapfile -t server_args <<<"$server_args_output"
    mapfile -t server_env <<<"$server_env_output"

    mkfifo "${TEMP_DIR}/in" "${TEMP_DIR}/out"
    env "${server_env[@]}" "$mcp_bin" "${server_args[@]}" <"${TEMP_DIR}/in" >"${TEMP_DIR}/out" 2>"${TEMP_DIR}/mcp.stderr" &
    MCP_PID=$!

    sleep 0.3

    if ! kill -0 "$MCP_PID" 2>/dev/null; then
        log_fail "MCP server failed to start"
        if [[ -s "${TEMP_DIR}/mcp.stderr" ]]; then
            cat "${TEMP_DIR}/mcp.stderr" >&2
        fi
        return 1
    fi

    exec 3>"${TEMP_DIR}/in"
    exec 4<"${TEMP_DIR}/out"
}

next_id() {
    REQUEST_ID=$((REQUEST_ID + 1))
    echo "$REQUEST_ID"
}

mcp_request() {
    local request="$1"
    local timeout="${2:-5}"
    local response

    echo "$request" >&3
    if ! read -r -t "$timeout" response <&4; then
        log_fail "Timeout waiting for MCP response"
        if [[ -s "${TEMP_DIR}/mcp.stderr" ]]; then
            cat "${TEMP_DIR}/mcp.stderr" >&2
        fi
        return 1
    fi
    echo "$response"
}

mcp_notify() {
    echo "$1" >&3
    sleep 0.1
}

assert_json() {
    local file="$1"
    local filter="$2"
    local message="$3"

    if jq -e "$filter" "$file" >/dev/null; then
        log_info "$message"
        return 0
    fi

    log_fail "$message"
    jq . "$file" >&2
    return 1
}

assert_json_arg() {
    local file="$1"
    local arg_name="$2"
    local arg_value="$3"
    local filter="$4"
    local message="$5"

    if jq -e --arg "$arg_name" "$arg_value" "$filter" "$file" >/dev/null; then
        log_info "$message"
        return 0
    fi

    log_fail "$message"
    jq . "$file" >&2
    return 1
}

test_generated_config_passthrough() {
    log_test "test_generated_config_passthrough: mkServerConfig serializes wrix options"

    new_temp_dir
    local user_data_dir="${TEMP_DIR}/user-data"
    local extra_config
    local config_file
    extra_config=$(jq -nc '{browser:{browserName:"firefox",launchOptions:{args:["--font-render-hinting=none"],channel:"chrome",executablePath:"/bad/chrome",headless:true,slowMo:0}},contextOptions:{acceptDownloads:false,viewport:{width:1,height:1}},metadata:{source:"passthrough"}}') || return 1
    config_file=$(playwright_config_path "$user_data_dir" false 1440 900 "$extra_config") || return 1

    assert_json "$config_file" '.browser.browserName == "chromium"' "browserName is pinned to chromium" || return 1
    assert_json_arg "$config_file" dir "$user_data_dir" ".browser.userDataDir == \$dir" "userDataDir reaches generated config" || return 1
    assert_json "$config_file" '.browser.launchOptions.headless == false' "headless option reaches launchOptions" || return 1
    assert_json "$config_file" '.browser.launchOptions.channel == "chromium"' "channel remains non-overridable" || return 1
    assert_json "$config_file" '.browser.launchOptions.executablePath | endswith("/chrome")' "chromium executable path is serialized" || return 1
    assert_json "$config_file" '.browser.launchOptions.args[0:3] == ["--no-sandbox","--disable-dev-shm-usage","--disable-gpu"]' "mandatory flags lead launchOptions.args" || return 1
    assert_json "$config_file" '.browser.launchOptions.args[3] == "--font-render-hinting=none"' "user launch args append after mandatory flags" || return 1
    assert_json "$config_file" '.browser.launchOptions.slowMo == 0' "other launchOptions fields pass through" || return 1
    assert_json "$config_file" '.contextOptions.viewport == {"width":1440,"height":900}' "viewport option reaches contextOptions" || return 1
    assert_json "$config_file" '.contextOptions.acceptDownloads == false' "contextOptions fields pass through" || return 1
    assert_json "$config_file" '.metadata.source == "passthrough"' "top-level config fields pass through" || return 1

    local chrome_path
    chrome_path=$(jq -r '.browser.launchOptions.executablePath' "$config_file") || return 1
    if [[ ! -x "$chrome_path" ]]; then
        log_fail "Configured chromium path is not executable: $chrome_path"
        return 1
    fi

    log_pass "test_generated_config_passthrough"
}

test_mcp_initialize() {
    log_test "test_mcp_initialize: MCP server starts from generated config and returns tools"

    new_temp_dir
    start_mcp_server "${TEMP_DIR}/user-data" || return 1

    local id response server_name tools tool_count
    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1.0\"}}}") || return 1
    server_name=$(echo "$response" | jq -r '.result.serverInfo.name') || return 1
    if [[ "$server_name" != "Playwright" ]]; then
        log_fail "Expected serverInfo.name='Playwright', got '$server_name'"
        return 1
    fi
    log_info "Server initialized: $server_name"

    mcp_notify '{"jsonrpc":"2.0","method":"notifications/initialized"}'

    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/list\"}") || return 1
    tools=$(echo "$response" | jq -r '[.result.tools[].name] | join(",")') || return 1

    local expected_tools=(
        "browser_navigate"
        "browser_take_screenshot"
        "browser_click"
        "browser_snapshot"
        "browser_console_messages"
    )

    local tool
    for tool in "${expected_tools[@]}"; do
        if [[ ",$tools," != *",$tool,"* ]]; then
            log_fail "Expected tool '$tool' not found in tool list"
            log_fail "Available tools: $tools"
            return 1
        fi
    done

    tool_count=$(echo "$response" | jq '.result.tools | length') || return 1
    log_info "Server reported $tool_count tools"

    stop_mcp_server
    log_pass "test_mcp_initialize"
}

test_offline_startup() {
    log_test "test_offline_startup: generated env disables browser downloads"

    new_temp_dir
    local env_json
    env_json=$(playwright_eval_raw server-env-json --argstr userDataDir "${TEMP_DIR}/user-data") || return 1
    if ! jq -e '.PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD == "1" and .PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS == "true"' <<<"$env_json" >/dev/null; then
        log_fail "Generated server env did not include offline Playwright settings"
        echo "$env_json" >&2
        return 1
    fi

    start_mcp_server "${TEMP_DIR}/user-data" || return 1

    local id response server_name error
    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1.0\"}}}") || return 1
    server_name=$(echo "$response" | jq -r '.result.serverInfo.name') || return 1
    if [[ "$server_name" != "Playwright" ]]; then
        log_fail "Server failed to start offline. Response: $response"
        return 1
    fi

    error=$(echo "$response" | jq -r '.error // empty') || return 1
    if [[ -n "$error" ]]; then
        log_fail "Server returned error on offline startup: $error"
        return 1
    fi

    stop_mcp_server
    log_pass "test_offline_startup"
}

main() {
    echo ""
    log_info "=========================================="
    log_info "  playwright-mcp Smoke Tests"
    log_info "=========================================="
    echo ""

    local passed=0
    local failed=0
    local test_fn

    for test_fn in test_generated_config_passthrough test_mcp_initialize test_offline_startup; do
        if "$test_fn"; then
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
