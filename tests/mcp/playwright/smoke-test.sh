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
MCP_EXTRA_ENV=()

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
    env "${MCP_EXTRA_ENV[@]}" "${server_env[@]}" "$mcp_bin" "${server_args[@]}" <"${TEMP_DIR}/in" >"${TEMP_DIR}/out" 2>"${TEMP_DIR}/mcp.stderr" &
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

assert_json_text() {
    local json="$1"
    local filter="$2"
    local message="$3"

    if jq -e "$filter" <<<"$json" >/dev/null; then
        log_info "$message"
        return 0
    fi

    log_fail "$message"
    jq . <<<"$json" >&2
    return 1
}

assert_representative_tools() {
    local response="$1"
    local tools
    local tool_count
    local expected_tools=(
        "browser_navigate"
        "browser_navigate_back"
        "browser_click"
        "browser_fill_form"
        "browser_select_option"
        "browser_hover"
        "browser_take_screenshot"
        "browser_snapshot"
        "browser_network_requests"
        "browser_tabs"
        "browser_console_messages"
    )
    local tool

    tools=$(echo "$response" | jq -r '[.result.tools[].name] | join(",")') || return 1
    for tool in "${expected_tools[@]}"; do
        if [[ ",$tools," != *",$tool,"* ]]; then
            log_fail "Expected tool '$tool' not found in tool list"
            log_fail "Available tools: $tools"
            return 1
        fi
    done

    tool_count=$(echo "$response" | jq '.result.tools | length') || return 1
    if ((tool_count < ${#expected_tools[@]})); then
        log_fail "Server returned fewer tools than the expected category coverage: $tool_count"
        return 1
    fi
    log_info "Server reported $tool_count tools"
}

test_chromium_executable_path_derives_from_playwright_browsers() {
    log_test "test_chromium_executable_path_derives_from_playwright_browsers: generated config uses packaged Chromium"

    new_temp_dir
    local user_data_dir="${TEMP_DIR}/user-data"
    local config_file
    local chrome_path
    local browsers_path
    local chrome_target
    config_file=$(playwright_config_path "$user_data_dir") || return 1
    chrome_path=$(jq -r '.browser.launchOptions.executablePath' "$config_file") || return 1
    browsers_path=$(playwright_build_package playwright-browsers) || return 1
    chrome_target=$(playwright_chromium_executable_target "$chrome_path") || return 1

    if ! playwright_chromium_path_is_derived_from_browsers "$chrome_path" "$browsers_path"; then
        log_fail "Chromium path is not derived from playwright-browsers: $chrome_path"
        log_fail "Chromium target: $chrome_target"
        log_fail "playwright-browsers path: $browsers_path"
        return 1
    fi
    log_info "Chromium path resolves under playwright-browsers"

    if [[ ! -x "$chrome_path" ]]; then
        log_fail "Configured chromium path is not executable: $chrome_path"
        return 1
    fi
    log_info "Chromium path is executable"

    log_pass "test_chromium_executable_path_derives_from_playwright_browsers"
}

test_chromium_executable_path_does_not_embed_linux_arch() {
    log_test "test_chromium_executable_path_does_not_embed_linux_arch: Linux configs use an architecture-neutral executable"

    local linux_system
    local config_json
    local chrome_path
    for linux_system in x86_64-linux aarch64-linux; do
        config_json=$(PLAYWRIGHT_SYSTEM="$linux_system" playwright_config_json "/tmp/wrix-playwright-${linux_system}") || return 1
        chrome_path=$(jq -r '.browser.launchOptions.executablePath' <<<"$config_json") || return 1

        if [[ "$chrome_path" == *"/chrome-linux64/chrome" || "$chrome_path" == *"/chrome-linux/chrome" ]]; then
            log_fail "Chromium path embeds a Linux browser archive layout for $linux_system: $chrome_path"
            return 1
        fi
        if [[ "$chrome_path" != /nix/store/*-playwright-chromium-executable/bin/chrome ]]; then
            log_fail "Chromium path for $linux_system does not use the derived executable helper: $chrome_path"
            return 1
        fi
        log_info "Chromium path for $linux_system is architecture-neutral"
    done

    log_pass "test_chromium_executable_path_does_not_embed_linux_arch"
}

test_mandatory_flags_are_non_overridable() {
    log_test "test_mandatory_flags_are_non_overridable: generated config prepends automatic Chromium flags"

    new_temp_dir
    local user_data_dir="${TEMP_DIR}/user-data"
    local extra_config
    local config_file
    extra_config=$(jq -nc '{launchOptions:{args:["--config-arg"],channel:"chrome",executablePath:"/bad/top",headless:true},browser:{launchOptions:{args:["--browser-arg"],channel:"chrome",executablePath:"/bad/browser",headless:true}}}') || return 1
    config_file=$(playwright_config_path "$user_data_dir" false 1280 720 "$extra_config") || return 1

    assert_json "$config_file" '.browser.launchOptions.args == ["--no-sandbox","--disable-dev-shm-usage","--disable-gpu","--config-arg","--browser-arg"]' "mandatory flags lead launchOptions.args and user args append" || return 1
    assert_json "$config_file" '.browser.launchOptions.channel == "chromium"' "channel remains non-overridable" || return 1
    assert_json "$config_file" '.browser.launchOptions.headless == false' "headless option cannot be overridden by config" || return 1
    assert_json "$config_file" '.browser.launchOptions.executablePath != "/bad/top" and .browser.launchOptions.executablePath != "/bad/browser"' "executablePath remains non-overridable" || return 1

    log_pass "test_mandatory_flags_are_non_overridable"
}

test_user_options_reach_serialized_config() {
    log_test "test_user_options_reach_serialized_config: headless viewport and passthrough config are serialized"

    new_temp_dir
    local user_data_dir="${TEMP_DIR}/user-data"
    local extra_config
    local config_file
    extra_config=$(jq -nc '{browser:{browserName:"firefox"},launchOptions:{slowMo:0},contextOptions:{acceptDownloads:false,viewport:{width:1,height:1}},metadata:{source:"passthrough"}}') || return 1
    config_file=$(playwright_config_path "$user_data_dir" false 1440 900 "$extra_config") || return 1

    assert_json "$config_file" '.browser.browserName == "chromium"' "browserName is pinned to chromium" || return 1
    assert_json_arg "$config_file" dir "$user_data_dir" ".browser.userDataDir == \$dir" "userDataDir reaches generated config" || return 1
    assert_json "$config_file" '.browser.launchOptions.headless == false' "headless option reaches launchOptions" || return 1
    assert_json "$config_file" '.browser.launchOptions.slowMo == 0' "launchOptions fields pass through" || return 1
    assert_json "$config_file" '.contextOptions.viewport == {"width":1440,"height":900}' "viewport option reaches contextOptions" || return 1
    assert_json "$config_file" '.contextOptions.acceptDownloads == false' "contextOptions fields pass through" || return 1
    assert_json "$config_file" '.metadata.source == "passthrough"' "top-level config fields pass through" || return 1

    log_pass "test_user_options_reach_serialized_config"
}

test_registry_triple_shape() {
    log_test "test_registry_triple_shape: server definition exposes the MCP registry triple"

    local registry_json
    registry_json=$(playwright_eval_raw server-registry-json) || return 1

    assert_json_text "$registry_json" '.name == "playwright"' "registry name is playwright" || return 1
    assert_json_text "$registry_json" '.packageNames | index("playwright-mcp")' "registry packages include playwright-mcp" || return 1
    assert_json_text "$registry_json" '.packageNames | index("playwright-browsers")' "registry packages include playwright-browsers" || return 1
    assert_json_text "$registry_json" '.packageNames | index("playwright-chromium-executable")' "registry packages include chromium executable helper" || return 1
    assert_json_text "$registry_json" '.mkServerConfigIsFunction == true' "mkServerConfig is a function" || return 1
    assert_json_text "$registry_json" '.sampleConfig.command == "playwright-mcp" and .sampleConfig.args[0] == "--config"' "mkServerConfig returns a Playwright MCP command with config args" || return 1

    log_pass "test_registry_triple_shape"
}

test_network_guard_blocks_ipv4_connect() {
    log_test "test_network_guard_blocks_ipv4_connect: deny helper blocks IPv4 connects"

    new_temp_dir
    local preload_dir
    local network_log
    local guard_env
    preload_dir=$(playwright_build_mode network-deny-preload) || return 1
    network_log="${TEMP_DIR}/network-attempts.log"
    : >"$network_log"
    guard_env=(
        "LD_PRELOAD=${preload_dir}/lib/libwrix-deny-network.so"
        "WRIX_NETWORK_DENY_LOG=$network_log"
    )

    if env "${guard_env[@]}" "$BASH" -c ': >/dev/tcp/127.0.0.1/9' 2>"${TEMP_DIR}/network-guard.stderr"; then
        log_fail "Network guard allowed an IPv4 connect"
        return 1
    fi

    if ! grep -q '^connect family=2$' "$network_log"; then
        log_fail "Network guard did not log the IPv4 connect"
        cat "$network_log" >&2
        if [[ -s "${TEMP_DIR}/network-guard.stderr" ]]; then
            cat "${TEMP_DIR}/network-guard.stderr" >&2
        fi
        return 1
    fi

    log_pass "test_network_guard_blocks_ipv4_connect"
}

test_mcp_initialize() {
    log_test "test_mcp_initialize: MCP server starts from generated config and returns tools"

    new_temp_dir
    start_mcp_server "${TEMP_DIR}/user-data" || return 1

    local id response server_name
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
    assert_representative_tools "$response" || return 1

    stop_mcp_server
    log_pass "test_mcp_initialize"
}

test_offline_startup() {
    log_test "test_offline_startup: startup succeeds with outbound network denied"

    new_temp_dir
    local env_json
    local preload_dir
    local network_log
    env_json=$(playwright_eval_raw server-env-json --argstr userDataDir "${TEMP_DIR}/user-data") || return 1
    if ! jq -e '.PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD == "1" and .PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS == "true"' <<<"$env_json" >/dev/null; then
        log_fail "Generated server env did not include offline Playwright settings"
        echo "$env_json" >&2
        return 1
    fi

    preload_dir=$(playwright_build_mode network-deny-preload) || return 1
    network_log="${TEMP_DIR}/network-attempts.log"
    : >"$network_log"
    MCP_EXTRA_ENV=(
        "LD_PRELOAD=${preload_dir}/lib/libwrix-deny-network.so"
        "WRIX_NETWORK_DENY_LOG=$network_log"
        "HTTP_PROXY=http://127.0.0.1:9"
        "HTTPS_PROXY=http://127.0.0.1:9"
        "ALL_PROXY=http://127.0.0.1:9"
        "NO_PROXY="
    )
    if ! start_mcp_server "${TEMP_DIR}/user-data"; then
        MCP_EXTRA_ENV=()
        if [[ -s "$network_log" ]]; then
            log_fail "Network attempts observed during offline startup"
            cat "$network_log" >&2
        fi
        return 1
    fi
    MCP_EXTRA_ENV=()

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

    mcp_notify '{"jsonrpc":"2.0","method":"notifications/initialized"}'

    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/list\"}") || return 1
    assert_representative_tools "$response" || return 1

    stop_mcp_server
    if [[ -s "$network_log" ]]; then
        log_fail "Network attempts observed during offline startup"
        cat "$network_log" >&2
        return 1
    fi
    log_info "Network guard observed no IPv4 or IPv6 startup attempts"
    log_pass "test_offline_startup"
}

ALL_TESTS=(
    test_chromium_executable_path_derives_from_playwright_browsers
    test_chromium_executable_path_does_not_embed_linux_arch
    test_mandatory_flags_are_non_overridable
    test_user_options_reach_serialized_config
    test_registry_triple_shape
    test_network_guard_blocks_ipv4_connect
    test_mcp_initialize
    test_offline_startup
)

main() {
    echo ""
    log_info "=========================================="
    log_info "  playwright-mcp Smoke Tests"
    log_info "=========================================="
    echo ""

    local passed=0
    local failed=0
    local test_fn
    local tests

    if (($# == 0)); then
        tests=("${ALL_TESTS[@]}")
    else
        tests=("$@")
    fi

    for test_fn in "${tests[@]}"; do
        if ! declare -F "$test_fn" >/dev/null; then
            log_fail "Unknown test function: $test_fn"
            failed=$((failed + 1))
            continue
        fi

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
