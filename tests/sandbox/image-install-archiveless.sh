#!/usr/bin/env bash
set -euo pipefail

skip() {
  local reason="$1"
  echo "SKIP: $reason" >&2
  exit 77
}

[[ "$(uname -s)" = "Linux" ]] || skip "Linux-only descriptor install verifier"
command -v nix >/dev/null 2>&1 || skip "nix not on PATH"

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
exec "$REPO_ROOT/tests/test-app.sh" test-image-install-archiveless "$@"
