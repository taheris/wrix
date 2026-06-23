#!/usr/bin/env bash
set -euo pipefail

WRIX_PREK_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WRIX_PREK_REPO_ROOT="${REPO_ROOT:-$(cd "$WRIX_PREK_LIB_DIR/../.." && pwd)}"

wrix_prek_skip() {
  local reason="$1"

  echo "SKIP: $reason" >&2
  exit 77
}

wrix_prek_require_tool() {
  local tool="$1"

  command -v "$tool" >/dev/null 2>&1 || wrix_prek_skip "$tool not on PATH"
}

wrix_prek_wrapper_bin() {
  local attr="$1"
  local binary="$2"
  local system result path

  wrix_prek_require_tool nix
  system=$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')
  result=$(nix build --no-link --print-out-paths --no-warn-dirty "$WRIX_PREK_REPO_ROOT#legacyPackages.$system.lib.$attr")
  path="$result/bin/$binary"

  if [[ ! -x "$path" ]]; then
    echo "FAIL: $path is not executable" >&2
    return 1
  fi

  printf '%s\n' "$path"
}
