#!/usr/bin/env bash
# Screenshot test for playwright-mcp
#
# Tests:
# 1. test_screenshot_returns_png: Navigate to a local HTTP server page,
#    take a screenshot, verify base64 PNG is returned
#
# Prerequisites:
# - playwright-mcp (from nixpkgs)
# - playwright-driver.browsers (chromium)
# - nodejs (for local HTTP server)
# - jq, base64, xxd

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
HTTP_PID=""
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
    if [[ -n "${HTTP_PID:-}" ]]; then
        kill "$HTTP_PID" 2>/dev/null || true
        wait "$HTTP_PID" 2>/dev/null || true
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

find_node() {
    if command -v node &>/dev/null; then
        command -v node
        return 0
    fi
    local pkg_path
    pkg_path=$(nix build 'nixpkgs#nodejs' --no-link --print-out-paths 2>/dev/null) || return 1
    echo "${pkg_path}/bin/node"
}

make_config() {
    local chrome_path="$1"
    local config_file="${TEMP_DIR}/config.json"
    # browserName + channel are pinned explicitly: @playwright/mcp defaults
    # `channel` to `"chrome-for-testing"` when neither browserName nor channel
    # is set, then tries to install that channel's binary under
    # $PLAYWRIGHT_BROWSERS_PATH and EACCES against the read-only nix store
    # path. Pinning channel=chromium honors executablePath without triggering
    # the install path. See playwright-core/lib/tools/mcp/config.js.
    local user_data_dir="${TEMP_DIR}/user-data"
    mkdir -p "$user_data_dir"
    # browserName + channel are pinned explicitly: @playwright/mcp defaults
    # `channel` to `"chrome-for-testing"` when neither is set. userDataDir is
    # pre-created in a writable temp dir because @playwright/mcp otherwise
    # calls createUserDataDir() which mkdirs `mcp-<channel>-<hash>` under
    # the playwright registry (the read-only nix store path), EACCES'ing on
    # the screenshot tool path. See
    # playwright-core/lib/tools/mcp/browserFactory.js:createUserDataDir.
    cat > "$config_file" <<EOF
{
  "browser": {
    "browserName": "chromium",
    "userDataDir": "${user_data_dir}",
    "launchOptions": {
      "args": ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"],
      "channel": "chromium",
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

start_http_server() {
    local node_bin="$1"
    local serve_dir="${TEMP_DIR}/www"
    mkdir -p "$serve_dir"
    cat > "${serve_dir}/index.html" <<'HTMLEOF'
<!DOCTYPE html>
<html>
<head><title>Screenshot Test</title></head>
<body style="margin:0;background:#2563eb;">
  <h1 style="color:white;padding:40px;font-family:sans-serif;">Playwright MCP Screenshot Test</h1>
</body>
</html>
HTMLEOF

    cat > "${TEMP_DIR}/server.js" <<'JSEOF'
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
  } catch (e) { res.writeHead(404); res.end("not found"); }
});
server.listen(0, "127.0.0.1", () => {
  fs.writeFileSync(process.argv[3], String(server.address().port));
});
JSEOF

    local port_file="${TEMP_DIR}/http_port"
    "$node_bin" "${TEMP_DIR}/server.js" "$serve_dir" "$port_file" &>/dev/null &
    HTTP_PID=$!

    # Wait for port file to appear
    local retries=20
    while [[ ! -f "$port_file" ]]; do
        ((retries--)) || { log_fail "HTTP server failed to start"; return 1; }
        sleep 0.2
    done

    cat "$port_file"
}

start_mcp_server() {
    local mcp_bin="$1"
    local config_file="$2"

    mkfifo "${TEMP_DIR}/in" "${TEMP_DIR}/out"

    PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 \
    PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true \
    "$mcp_bin" --config "$config_file" < "${TEMP_DIR}/in" > "${TEMP_DIR}/out" 2>/dev/null &
    MCP_PID=$!

    sleep 0.3

    if ! kill -0 "$MCP_PID" 2>/dev/null; then
        log_fail "MCP server failed to start"
        return 1
    fi

    exec 3>"${TEMP_DIR}/in"
    exec 4<"${TEMP_DIR}/out"
}

next_id() {
    ((REQUEST_ID++))
    echo "$REQUEST_ID"
}

mcp_request() {
    local request="$1"
    local timeout="${2:-10}"
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

test_screenshot_returns_png() {
    log_test "test_screenshot_returns_png: Navigate and capture screenshot as base64 PNG"

    local mcp_bin chrome_path node_bin config_file
    mcp_bin=$(find_playwright_mcp) || { log_fail "Cannot find playwright-mcp"; return 1; }
    chrome_path=$(find_chromium) || { log_fail "Cannot find chromium"; return 1; }
    node_bin=$(find_node) || { log_fail "Cannot find node"; return 1; }

    TEMP_DIR=$(mktemp -d)
    config_file=$(make_config "$chrome_path")

    # Start HTTP server with test page
    local port
    port=$(start_http_server "$node_bin") || return 1
    log_info "HTTP server on port $port"

    # Start MCP server
    start_mcp_server "$mcp_bin" "$config_file" || return 1

    # Initialize
    local id response
    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1.0\"}}}")

    local server_name
    server_name=$(echo "$response" | jq -r '.result.serverInfo.name')
    if [[ "$server_name" != "Playwright" ]]; then
        log_fail "Initialize failed: $response"
        return 1
    fi
    log_info "Server initialized"

    mcp_notify '{"jsonrpc":"2.0","method":"notifications/initialized"}'

    # Navigate to the test page
    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"browser_navigate\",\"arguments\":{\"url\":\"http://127.0.0.1:${port}/\"}}}" 30)

    local nav_error
    nav_error=$(echo "$response" | jq -r '.error // empty')
    if [[ -n "$nav_error" ]]; then
        log_fail "Navigate failed: $nav_error"
        return 1
    fi
    log_info "Navigated to http://127.0.0.1:${port}/"

    # Take screenshot
    id=$(next_id)
    response=$(mcp_request "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"tools/call\",\"params\":{\"name\":\"browser_take_screenshot\",\"arguments\":{}}}" 30)

    local call_error
    call_error=$(echo "$response" | jq -r '.error // empty')
    if [[ -n "$call_error" ]]; then
        log_fail "Screenshot call failed: $call_error"
        return 1
    fi

    # The screenshot result should contain base64 PNG data in the content array
    # Content items with type "image" have base64-encoded data
    local b64_data
    b64_data=$(echo "$response" | jq -r '.result.content[] | select(.type == "image") | .data' 2>/dev/null)

    if [[ -z "$b64_data" ]]; then
        log_fail "No image data found in screenshot response"
        log_fail "Response content: $(echo "$response" | jq -c '.result.content')"
        return 1
    fi

    # Verify PNG magic bytes: PNG files start with 0x89 0x50 0x4E 0x47 0x0D 0x0A 0x1A 0x0A
    local decoded_header
    decoded_header=$(echo "$b64_data" | base64 -d 2>/dev/null | head -c 8 | xxd -p)

    if [[ "$decoded_header" != 89504e470d0a1a0a ]]; then
        log_fail "Decoded data does not have PNG magic bytes"
        log_fail "Got header: $decoded_header"
        return 1
    fi
    log_info "Screenshot contains valid PNG data"

    # Verify image is non-trivially sized (at least 1KB of base64 data)
    local data_len=${#b64_data}
    if [[ $data_len -lt 1000 ]]; then
        log_fail "Screenshot data suspiciously small: $data_len bytes"
        return 1
    fi
    log_info "Screenshot size: $data_len base64 characters"

    log_pass "test_screenshot_returns_png"
}

# --- Main ---

main() {
    echo ""
    log_info "=========================================="
    log_info "  playwright-mcp Screenshot Tests"
    log_info "=========================================="
    echo ""

    local passed=0
    local failed=0

    # shellcheck disable=SC2043  # single-element loop; more tests will be added
    for test_fn in test_screenshot_returns_png; do
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
