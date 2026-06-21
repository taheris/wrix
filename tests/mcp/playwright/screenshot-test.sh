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
HTTP_PID=""
TEMP_DIR=""
REQUEST_ID=0

cleanup() {
    local exit_code=$?
    stop_mcp_server
    stop_http_server
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

stop_http_server() {
    if [[ -n "${HTTP_PID:-}" ]]; then
        kill "$HTTP_PID" 2>/dev/null || true # best-effort: process may have exited.
        wait "$HTTP_PID" 2>/dev/null || true # best-effort: process may already be reaped.
        HTTP_PID=""
    fi
}

start_http_server() {
    local node_bin="$1"
    local serve_dir="${TEMP_DIR}/www"
    local port_file="${TEMP_DIR}/http_port"
    local retries=20

    mkdir -p "$serve_dir"
    cat >"${serve_dir}/index.html" <<'HTML'
<!DOCTYPE html>
<html>
<head><title>Screenshot Test</title></head>
<body style="margin:0;background:#2563eb;">
  <h1 style="color:white;padding:40px;font-family:sans-serif;">Playwright MCP Screenshot Test</h1>
</body>
</html>
HTML

    cat >"${TEMP_DIR}/server.js" <<'JS'
const http = require("http");
const fs = require("fs");
const path = require("path");
const dir = process.argv[2];
const server = http.createServer((req, res) => {
  const file = path.join(dir, req.url === "/" ? "index.html" : req.url);
  try {
    const data = fs.readFileSync(file);
    res.writeHead(200, {"Content-Type": "text/html"});
    res.end(data);
  } catch {
    res.writeHead(404);
    res.end("not found");
  }
});
server.listen(0, "127.0.0.1", () => {
  fs.writeFileSync(process.argv[3], String(server.address().port));
});
JS

    "$node_bin" "${TEMP_DIR}/server.js" "$serve_dir" "$port_file" >"${TEMP_DIR}/http.stdout" 2>"${TEMP_DIR}/http.stderr" &
    HTTP_PID=$!

    while [[ ! -f "$port_file" ]]; do
        retries=$((retries - 1))
        if ((retries == 0)); then
            log_fail "HTTP server failed to start"
            if [[ -s "${TEMP_DIR}/http.stderr" ]]; then
                cat "${TEMP_DIR}/http.stderr" >&2
            fi
            return 1
        fi
        sleep 0.2
    done

    cat "$port_file"
}

start_mcp_server() {
    local user_data_dir="$1"
    local mcp_bin
    local server_args_output
    local server_env_output
    local server_args
    local server_env

    mkdir -p "$user_data_dir"
    mcp_bin=$(playwright_find_mcp) || return 1
    server_args_output=$(playwright_server_args "$user_data_dir") || return 1
    server_env_output=$(playwright_server_env "$user_data_dir") || return 1
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
    local timeout="${2:-10}"
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

test_screenshot_returns_png() {
    log_test "test_screenshot_returns_png: generated config navigates and captures PNG"

    new_temp_dir
    local node_bin port id response server_name nav_error call_error b64_data decoded_file decoded_header data_len
    node_bin=$(playwright_find_node) || return 1
    port=$(start_http_server "$node_bin") || return 1
    log_info "HTTP server on port $port"

    start_mcp_server "${TEMP_DIR}/user-data" || return 1

    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1.0\"}}}") || return 1
    server_name=$(echo "$response" | jq -r '.result.serverInfo.name') || return 1
    if [[ "$server_name" != "Playwright" ]]; then
        log_fail "Initialize failed: $response"
        return 1
    fi
    log_info "Server initialized"

    mcp_notify '{"jsonrpc":"2.0","method":"notifications/initialized"}'

    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"browser_navigate\",\"arguments\":{\"url\":\"http://127.0.0.1:${port}/\"}}}" 30) || return 1
    nav_error=$(echo "$response" | jq -r '.error // empty') || return 1
    if [[ -n "$nav_error" ]]; then
        log_fail "Navigate failed: $nav_error"
        return 1
    fi
    log_info "Navigated to http://127.0.0.1:${port}/"

    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"browser_take_screenshot\",\"arguments\":{}}}" 30) || return 1
    call_error=$(echo "$response" | jq -r '.error // empty') || return 1
    if [[ -n "$call_error" ]]; then
        log_fail "Screenshot call failed: $call_error"
        return 1
    fi

    b64_data=$(echo "$response" | jq -r '.result.content[] | select(.type == "image") | .data') || return 1
    if [[ -z "$b64_data" ]]; then
        log_fail "No image data found in screenshot response"
        log_fail "Response content: $(echo "$response" | jq -c '.result.content')"
        return 1
    fi

    decoded_file="${TEMP_DIR}/screenshot.png"
    printf '%s' "$b64_data" | base64 -d >"$decoded_file" || return 1
    decoded_header=$(head -c 8 "$decoded_file" | xxd -p) || return 1
    if [[ "$decoded_header" != 89504e470d0a1a0a ]]; then
        log_fail "Decoded data does not have PNG magic bytes"
        log_fail "Got header: $decoded_header"
        return 1
    fi
    log_info "Screenshot contains valid PNG data"

    data_len=${#b64_data}
    if ((data_len < 1000)); then
        log_fail "Screenshot data suspiciously small: $data_len bytes"
        return 1
    fi
    log_info "Screenshot size: $data_len base64 characters"

    log_pass "test_screenshot_returns_png"
}

main() {
    echo ""
    log_info "=========================================="
    log_info "  playwright-mcp Screenshot Tests"
    log_info "=========================================="
    echo ""

    local passed=0
    local failed=0

    if test_screenshot_returns_png; then
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
