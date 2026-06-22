#!/usr/bin/env bash
set -euo pipefail

skip() {
  local reason="$1"
  echo "SKIP: $reason" >&2
  exit 77
}

[[ "$(uname -s)" = "Linux" ]] || skip "Linux-only descriptor install verifier"
command -v nix >/dev/null 2>&1 || skip "nix not on PATH"

exec nix run --no-warn-dirty .#test-image-install-archiveless -- "$@"
