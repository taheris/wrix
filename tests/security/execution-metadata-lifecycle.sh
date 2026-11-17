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
case "$(uname -s)" in
  Darwin)
    command -v cargo >/dev/null 2>&1 || wrix_live_skip "cargo not on PATH"
    exec cargo test --offline --locked --manifest-path "$REPO_ROOT/Cargo.toml" \
      --package wrix-verify-tools --test execution_lifecycle -- --include-ignored --test-threads=1
    ;;
  Linux)
    command -v python3 >/dev/null 2>&1 || wrix_live_skip "python3 not on PATH"
    exec python3 "$SCRIPT_DIR/test_execution_lifecycle.py" live
    ;;
esac
