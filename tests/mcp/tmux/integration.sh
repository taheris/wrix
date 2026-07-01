#!/usr/bin/env bash
# Verifier for the tmux-mcp integration suite (specs/tmux-mcp.md).
#
# Drives the existing tests/mcp/tmux/test_*.sh scripts directly against
# the host's tmux + the wrix-built tmux-mcp binary — no container, no
# VM. Each test exercises the MCP server's JSON-RPC surface over stdin/
# stdout for one tool (create_pane, send_keys, capture_pane, kill_pane,
# list_panes, exited_pane, error_handling, audit_log, audit_full_capture,
# cleanup_on_exit).
#
#   Linux + nix + tmux on PATH  -> exercise the binary
#   non-Linux                   -> exit 77 (test_*.sh assume Linux tmux semantics)
#   nix or tmux missing         -> exit 77

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

uname_s=$(uname -s)
[[ "$uname_s" = "Linux" ]] || skip "Linux-only verifier (uname=$uname_s); test_*.sh use Linux tmux semantics"
command -v nix  >/dev/null 2>&1 || skip "nix not on PATH"
command -v tmux >/dev/null 2>&1 || skip "tmux not on PATH"

cd "$REPO_ROOT"

build_log=$(mktemp -t wrix-tmux-mcp-build.XXXXXX)
if ! TMUX_MCP_BIN_DIR=$(nix build --no-link --print-out-paths --no-warn-dirty .#tmux-mcp 2>"$build_log"); then
  cat "$build_log" >&2
  rm -f "$build_log"
  exit 1
fi
rm -f "$build_log"

PATH="$TMUX_MCP_BIN_DIR/bin:$PATH" exec bash "$REPO_ROOT/tests/mcp/tmux/run-integration.sh" "$@"
