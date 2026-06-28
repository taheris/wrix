#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  local reason="$1"

  echo "SKIP: $reason" >&2
  exit 77
}

command -v nix >/dev/null 2>&1 || skip "nix not on PATH"
command -v jq >/dev/null 2>&1 || skip "jq not on PATH"

SYSTEM="$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')"
APPS_JSON="$(nix eval --json --no-warn-dirty --apply 'builtins.attrNames' "$REPO_ROOT#apps.$SYSTEM")"
CHECKS_JSON="$(nix eval --json --no-warn-dirty --apply 'builtins.attrNames' "$REPO_ROOT#checks.$SYSTEM")"
CI_CHECKS_JSON="$(nix eval --json --no-warn-dirty --apply 'builtins.attrNames' "$REPO_ROOT#legacyPackages.$SYSTEM.ciChecks")"
CHECKS_DETAILS_JSON="$(nix eval --json --no-warn-dirty --apply '
  checks:
    builtins.mapAttrs
      (name: drv: {
        attr = name;
        drvName = drv.name or "";
        pname = drv.pname or "";
      })
      checks
' "$REPO_ROOT#checks.$SYSTEM")"
TEST_CI_ROOT="$(nix build --no-link --print-out-paths --no-warn-dirty --impure --expr "
let
  app = (builtins.getFlake \"git+file://$REPO_ROOT\").apps.\"$SYSTEM\".\"test-ci\";
  program = app.program;
  suffix = \"/bin/test-ci\";
in builtins.substring 0 ((builtins.stringLength program) - (builtins.stringLength suffix)) program
")"
TEST_CI_PROGRAM="$TEST_CI_ROOT/bin/test-ci"

CI_APPS=(
  test-wrix-spawn-load
  test-image-install-archiveless
  test-image-install-real-skopeo
  test-image-install-digest-skip
  test-image-digest-matches-stored-id
  test-linux-image-archiveless-source
  test-image-digest-no-tar
  test-image-tier-graph
  test-image-tier-membership
  test-wrix-images-source-kind
  test-wrix-image-labels
  test-claude-runtime-noop
  test-prek-hooks-closure
  test-base-image-universal
  test-entrypoint-resolver-base
  test-base-image-hash-stable
  test-stable-profile-hash-stable
  test-stable-profile-membership
  test-pinned-toolchain-stable-tier
  test-downstream-change-leaf-only
  test-archiveless-generated-change
  test-agent-tier-isolated
  test-agent-exclusive
  test-iteration-cost-bounded
  test-customisation-layer-bounded
  test-image-nix-db-consistent
  test-image-nix-db-no-dangling
  test-profiles-build-package
)

CI_CHECKS=(
  tmux-mcp-clippy
  tmux-mcp-nextest
  wrix-rust-clippy
  wrix-rust-nextest
  image-builds
  package-script-syntax
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

CI_CHECK_MARKER="legacyPackages.$SYSTEM.ciChecks.\$check"
if ! grep -qF "$CI_CHECK_MARKER" "$TEST_CI_PROGRAM"; then
  echo "FAIL: test-ci does not build CI-only heavy checks" >&2
  failed=$((failed + 1))
fi

FORBIDDEN_HEAVY_CHECK_RE='(^|-)wrix-rust-(clippy|nextest)$|(^|-)tmux-mcp-(clippy|nextest)$|(^|-)image-builds$|(^|-)package-script-syntax$'
forbidden_attrs="$(jq --arg re "$FORBIDDEN_HEAVY_CHECK_RE" '[.[] | select(test($re))]' <<<"$CHECKS_JSON")"
if [[ "$(jq 'length' <<<"$forbidden_attrs")" -ne 0 ]]; then
  echo "FAIL: checks.$SYSTEM contains CI-only heavy checks: $(jq -r 'join(", ")' <<<"$forbidden_attrs")" >&2
  failed=$((failed + 1))
fi

forbidden_drvs="$(jq --arg re "$FORBIDDEN_HEAVY_CHECK_RE" '[to_entries[] | select((.value.drvName // "") | test($re)) | .key]' <<<"$CHECKS_DETAILS_JSON")"
if [[ "$(jq 'length' <<<"$forbidden_drvs")" -ne 0 ]]; then
  echo "FAIL: checks.$SYSTEM contains CI-only heavy derivations: $(jq -r 'join(", ")' <<<"$forbidden_drvs")" >&2
  failed=$((failed + 1))
fi

for app in "${CI_APPS[@]}"; do
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

for check in "${CI_CHECKS[@]}"; do
  if ! jq -e --arg check "$check" 'index($check)' <<<"$CI_CHECKS_JSON" >/dev/null; then
    echo "FAIL: legacyPackages.$SYSTEM.ciChecks.$check is missing" >&2
    failed=$((failed + 1))
  fi
  if jq -e --arg check "$check" 'index($check)' <<<"$CHECKS_JSON" >/dev/null; then
    echo "FAIL: checks.$SYSTEM.$check should be CI-only" >&2
    failed=$((failed + 1))
  fi
  if ! awk -v check="$check" '
    {
      line = $0
      sub(/[[:space:]]*\\$/, "", line)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
      if (line == check) found = 1
    }
    END { exit found ? 0 : 1 }
  ' "$TEST_CI_PROGRAM"; then
    echo "FAIL: test-ci does not build legacyPackages.$SYSTEM.ciChecks.$check" >&2
    failed=$((failed + 1))
  fi
done

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

echo "PASS: heavy realization checks are CI-only apps/checks"
