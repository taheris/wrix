#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"
wrix_require_live_sandbox
command -v script >/dev/null 2>&1 || wrix_live_skip "script not on PATH"
case "$(uname -s)" in
  Linux) timeout 30 podman info >/dev/null || wrix_live_skip "Podman runtime unavailable or timed out" ;;
  Darwin) timeout 30 container system status || wrix_live_skip "Apple container service unavailable or timed out" ;;
esac
cd "$REPO_ROOT"
: "${WRIX_EXECUTION_HARNESS:?the owning CI app must supply its native test executable}"
[[ "$WRIX_EXECUTION_HARNESS" = /* && -x "$WRIX_EXECUTION_HARNESS" ]] || {
  printf 'Invalid native execution lifecycle harness.\n' >&2
  exit 1
}
exec "$WRIX_EXECUTION_HARNESS" --include-ignored --test-threads=1
