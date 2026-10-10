#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"
wrix_require_live_sandbox
for tool in python3 script; do
  command -v "$tool" >/dev/null 2>&1 || wrix_live_skip "$tool not on PATH"
done
case "$(uname -s)" in
  Linux) timeout 30 podman info >/dev/null || wrix_live_skip "Podman runtime unavailable or timed out" ;;
  Darwin) timeout 30 container system status || wrix_live_skip "Apple container service unavailable or timed out" ;;
esac
cd "$REPO_ROOT"
exec python3 "$SCRIPT_DIR/test_execution_lifecycle.py" live
