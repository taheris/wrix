#!/usr/bin/env bash
# Verifier for tmux-mcp's mkSandbox composition (specs/tmux-mcp.md).
#
# Drives the NixOS VM check `tmux-mcp-e2e-sandbox` (tests/mcp/tmux/check.nix),
# which loads the rust-debug profile image into a rootless podman inside a
# VM and asserts tmux + tmux-mcp resolve on PATH and the MCP server responds
# to a JSON-RPC initialize.
#
#   Linux with /dev/kvm + nix  -> nix build .#checks.<system>.tmux-mcp-e2e-sandbox
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
    skip "tmux-mcp-e2e-sandbox runs in a NixOS VM (Linux-only)"
  fi

  if [[ "$uname_s" != "Linux" ]]; then
    skip "unsupported platform: $uname_s"
  fi

  if [[ ! -e /dev/kvm ]]; then
    skip "/dev/kvm not present — flake check tmux-mcp-e2e-sandbox is gated on KVM"
  fi

  if ! command -v nix >/dev/null 2>&1; then
    skip "nix not on PATH — cannot drive .#checks.<system>.tmux-mcp-e2e-sandbox"
  fi

  local system
  system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')

  local attr=".#checks.${system}.tmux-mcp-e2e-sandbox"
  echo "=== nix build $attr ===" >&2
  # --impure: tests/mcp/tmux/check.nix gates e2eTests on
  # `builtins.pathExists "/dev/kvm"`, which returns false in pure eval.
  if ! nix build --impure --no-link --print-build-logs --no-warn-dirty "$attr"; then
    echo "FAIL: $attr did not build" >&2
    return 1
  fi
  echo "PASS: $attr built successfully" >&2
}

(cd "$REPO_ROOT" && main "$@")
