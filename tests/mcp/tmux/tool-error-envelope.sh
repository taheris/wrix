#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"
TMUX_MCP_BIN="${TMUX_MCP_BIN:-}"

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

skip() {
  local message="$1"
  printf 'SKIP: %s\n' "$message" >&2
  exit 77
}

require_command() {
  local command_name="$1"
  if ! command -v "$command_name" >/dev/null 2>&1; then
    skip "$command_name is required"
  fi
}

build_tmux_mcp() {
  if [[ -z "$TMUX_MCP_BIN" ]]; then
    local bin_dir
    local build_log
    build_log=$(mktemp -t wrix-tmux-mcp-build.XXXXXX) || return 1
    if ! bin_dir=$(nix build --no-link --print-out-paths --no-warn-dirty .#tmux-mcp 2>"$build_log"); then
      cat "$build_log" >&2
      rm -f "$build_log"
      return 1
    fi
    rm -f "$build_log"
    TMUX_MCP_BIN="$bin_dir/bin/tmux-mcp"
  fi
  printf '%s\n' "$TMUX_MCP_BIN"
}

run_tool_error_call() {
  local tmux_mcp
  tmux_mcp=$(build_tmux_mcp) || return 1

  local stderr_file
  stderr_file=$(mktemp -t tmux-mcp-error-envelope-stderr.XXXXXX) || return 1

  local output
  if output=$(timeout 10 "$tmux_mcp" 2>"$stderr_file" <<'JSON'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tmux_send_keys","arguments":{"pane_id":"debug-999","keys":"echo hello"}}}
JSON
  ); then
    :
  else
    local status="$?"
    local stderr
    stderr=$(<"$stderr_file")
    rm -f "$stderr_file"
    fail "tmux-mcp exited with status $status: $stderr"
    return 1
  fi

  rm -f "$stderr_file"

  local line_count
  line_count=$(printf '%s\n' "$output" | sed '/^$/d' | wc -l | tr -d ' ') || return 1
  if [[ "$line_count" != "2" ]]; then
    fail "expected initialize and tool-call responses, got $line_count line(s): $output"
    return 1
  fi

  printf '%s\n' "$output" | tail -n 1
}

assert_jq() {
  local json="$1"
  local filter="$2"
  local message="$3"
  if ! jq -e "$filter" >/dev/null <<<"$json"; then
    fail "$message: $json"
  fi
}

test_tool_handler_error_response_uses_mcp_success_envelope() {
  local response
  response=$(run_tool_error_call) || return 1

  assert_jq "$response" '.error == null' 'tool error must be a JSON-RPC success response' || return 1
  assert_jq "$response" '.result.isError == true' 'tool error result must set isError true' || return 1
  assert_jq "$response" '.result.content | type == "array" and length == 1' 'tool error result must contain one content item' || return 1
  assert_jq "$response" '.result.content[0].type == "text"' 'tool error content must be text' || return 1
  assert_jq "$response" '.result.content[0].text | contains("not found")' 'tool error text must describe the failure'
}

test_tool_handler_error_has_no_custom_error_code_field() {
  local response
  response=$(run_tool_error_call) || return 1

  assert_jq "$response" 'any(.. | objects; has("errorCode") or has("error_code")) | not' 'tool error envelope must not include a custom error-code field'
}

ALL_TESTS=(
  test_tool_handler_error_response_uses_mcp_success_envelope
  test_tool_handler_error_has_no_custom_error_code_field
)

run_all() {
  local failed=0
  local fn
  for fn in "${ALL_TESTS[@]}"; do
    printf '=== %s ===\n' "$fn"
    if "$fn"; then
      printf 'PASS: %s\n' "$fn"
    else
      printf 'FAIL: %s\n' "$fn" >&2
      failed=$((failed + 1))
    fi
  done
  if [[ "$failed" -ne 0 ]]; then
    printf '%s test(s) failed\n' "$failed" >&2
    return 1
  fi
}

require_command jq
require_command nix
require_command timeout
cd "$REPO_ROOT"

if [[ "$#" -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    printf 'Unknown function: %s\n' "$fn" >&2
    exit 1
  fi
  "$fn"
fi
