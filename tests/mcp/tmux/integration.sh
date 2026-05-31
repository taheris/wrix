#!/usr/bin/env bash
# Verifier for the tmux-mcp integration suite (specs/tmux-mcp.md).
#
# Drives the NixOS VM check `tmux-mcp-integration` (tests/mcp/tmux/check.nix),
# which boots a VM with tmux + tmux-mcp on PATH and runs every `test_*.sh`
# under tests/mcp/tmux/ against the MCP server over JSON-RPC. The covered
# behaviours are pane lifecycle (create/list/kill), send_keys + capture
# round-trip, exited-pane reporting, audit log JSON-Lines format, error
# handling, and session cleanup on server exit.
#
#   Linux with /dev/kvm + nix  -> nix build .#checks.<system>.tmux-mcp-integration
#   Darwin                     -> exit 77 (NixOS VM is Linux-only)
#   Linux without /dev/kvm     -> exit 77 (check is gated on KVM)
#   nix unavailable on PATH    -> exit 77

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

main() {
  local uname_s
  uname_s=$(uname -s)

  if [[ "$uname_s" = "Darwin" ]]; then
    skip "tmux-mcp-integration runs in a NixOS VM (Linux-only)"
  fi

  if [[ "$uname_s" != "Linux" ]]; then
    skip "unsupported platform: $uname_s"
  fi

  if [[ ! -e /dev/kvm ]]; then
    skip "/dev/kvm not present — flake check tmux-mcp-integration is gated on KVM"
  fi

  if ! command -v nix >/dev/null 2>&1; then
    skip "nix not on PATH — cannot drive .#checks.<system>.tmux-mcp-integration"
  fi

  local system
  system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')

  local attr=".#checks.${system}.tmux-mcp-integration"
  echo "=== nix build $attr ===" >&2
  # --impure: tests/mcp/tmux/check.nix gates integrationTests on
  # `builtins.pathExists "/dev/kvm"`, which returns false in pure eval
  # even when /dev/kvm exists. The shell-level KVM guard above already
  # gates the script.
  if ! nix build --impure --no-link --print-build-logs --no-warn-dirty "$attr"; then
    echo "FAIL: $attr did not build" >&2
    return 1
  fi
  echo "PASS: $attr built successfully" >&2
}

(cd "$REPO_ROOT" && main "$@")
