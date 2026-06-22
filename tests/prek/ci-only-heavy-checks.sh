#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

command -v nix >/dev/null 2>&1 || skip "nix not on PATH"
command -v jq >/dev/null 2>&1 || skip "jq not on PATH"

SYSTEM="$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')"
APPS_JSON="$(nix eval --json --no-warn-dirty --apply 'builtins.attrNames' "$REPO_ROOT#apps.$SYSTEM")"
CHECKS_JSON="$(nix eval --json --no-warn-dirty --apply 'builtins.attrNames' "$REPO_ROOT#checks.$SYSTEM")"
TEST_CI_ROOT="$(nix build --no-link --print-out-paths --no-warn-dirty --impure --expr "
let
  app = (builtins.getFlake \"git+file://$REPO_ROOT\").apps.\"$SYSTEM\".\"test-ci\";
  program = app.program;
  suffix = \"/bin/test-ci\";
in builtins.substring 0 ((builtins.stringLength program) - (builtins.stringLength suffix)) program
")"
TEST_CI_PROGRAM="$TEST_CI_ROOT/bin/test-ci"

HEAVY_APPS=(
  test-wrix-spawn-load
  test-image-install-archiveless
  test-image-install-digest-skip
  test-image-digest-matches-stored-id
  test-claude-runtime-noop
  test-prek-hooks-closure
  test-base-image-universal
  test-entrypoint-resolver-base
  test-base-image-hash-stable
  test-stable-profile-hash-stable
  test-stable-profile-membership
  test-pinned-toolchain-stable-tier
  test-downstream-change-leaf-only
  test-agent-tier-isolated
  test-agent-exclusive
  test-iteration-cost-bounded
  test-customisation-layer-bounded
  test-image-nix-db-consistent
  test-image-nix-db-no-dangling
  test-profiles-build-package
)

failed=0
if ! jq -e 'index("test-ci")' <<<"$APPS_JSON" >/dev/null; then
  echo "FAIL: apps.$SYSTEM.test-ci is missing" >&2
  failed=$((failed + 1))
fi
NIX_RUN_MARKER="nix run --no-warn-dirty \".#\$app\""
if ! grep -qF "$NIX_RUN_MARKER" "$TEST_CI_PROGRAM"; then
  echo "FAIL: test-ci does not run CI apps through nix run" >&2
  failed=$((failed + 1))
fi

for app in "${HEAVY_APPS[@]}"; do
  if ! jq -e --arg app "$app" 'index($app)' <<<"$APPS_JSON" >/dev/null; then
    echo "FAIL: apps.$SYSTEM.$app is missing" >&2
    failed=$((failed + 1))
  fi
  if jq -e --arg app "$app" 'index($app)' <<<"$CHECKS_JSON" >/dev/null; then
    echo "FAIL: checks.$SYSTEM.$app should be CI-only" >&2
    failed=$((failed + 1))
  fi
  if ! awk -v app="$app" '
    {
      line = $0
      sub(/[[:space:]]*\\$/, "", line)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
      if (line == app) found = 1
    }
    END { exit found ? 0 : 1 }
  ' "$TEST_CI_PROGRAM"; then
    echo "FAIL: test-ci does not invoke .#$app" >&2
    failed=$((failed + 1))
  fi
done

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

echo "PASS: heavy realization checks are CI-only apps"
