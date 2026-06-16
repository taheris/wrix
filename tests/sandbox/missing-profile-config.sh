#!/usr/bin/env bash
set -euo pipefail

if ! command -v nix >/dev/null 2>&1; then
  echo "skip: nix not on PATH" >&2
  exit 77
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP=$(mktemp -d -t wrix-missing-profile-config.XXXXXX)
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

PASSED=0
FAILED=0

pass() { printf '  PASS: %s\n' "$1"; PASSED=$((PASSED + 1)); }
fail() { printf '  FAIL: %s\n' "$1" >&2; FAILED=$((FAILED + 1)); }

LAUNCHER_LINK="$TEST_TMP/launcher"
nix build \
  --impure --no-warn-dirty \
  --out-link "$LAUNCHER_LINK" \
  --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      system = builtins.currentSystem;
      lib = flake.legacyPackages.\${system}.lib;
    in (lib.mkSandbox { profile = lib.profiles.base; }).launcher
  "

WRIX="$LAUNCHER_LINK/bin/wrix"
WORKSPACE="$TEST_TMP/workspace"
mkdir -p "$WORKSPACE"

run_wrix() {
  local out="$1"
  local err="$2"
  shift 2
  local rc=0
  WRIX_DRY_RUN=1 "$WRIX" "$@" >"$out" 2>"$err" || rc=$?
  echo "$rc"
}

test_run_requires_profile_config() {
  local out="$TEST_TMP/no-config.out" err="$TEST_TMP/no-config.err" rc
  rc=$(run_wrix "$out" "$err" run "$WORKSPACE")
  if [[ "$rc" == "0" ]]; then
    fail "wrix run without --profile-config exited 0"
    return 1
  fi
  if ! grep -qF -- '--profile-config' "$err" || ! grep -qF 'ProfileConfig' "$err"; then
    fail "missing-config error did not name --profile-config and ProfileConfig: $(cat "$err")"
    return 1
  fi
  pass "wrix run fails clearly when --profile-config is absent"
}

test_run_rejects_invalid_profile_config_json() {
  local bad="$TEST_TMP/bad-profile.json"
  printf '{not-json\n' > "$bad"

  local out="$TEST_TMP/bad.out" err="$TEST_TMP/bad.err" rc
  rc=$(run_wrix "$out" "$err" --profile-config "$bad" run "$WORKSPACE")
  if [[ "$rc" == "0" ]]; then
    fail "wrix run accepted invalid ProfileConfig JSON"
    return 1
  fi
  if ! grep -qF 'invalid ProfileConfig JSON' "$err"; then
    fail "invalid-json error was not clear: $(cat "$err")"
    return 1
  fi
  pass "wrix run rejects invalid ProfileConfig JSON with a clear error"
}

test_run_rejects_wrong_profile_config_schema() {
  local bad="$TEST_TMP/schema-profile.json"
  cat > "$bad" <<'JSON'
{"schema":2,"image":{"ref":"wrix:test","source":"/nix/store/fake"},"agent":{"kind":"direct"}}
JSON

  local out="$TEST_TMP/schema.out" err="$TEST_TMP/schema.err" rc
  rc=$(run_wrix "$out" "$err" --profile-config "$bad" run "$WORKSPACE")
  if [[ "$rc" == "0" ]]; then
    fail "wrix run accepted unsupported ProfileConfig schema"
    return 1
  fi
  if ! grep -qF 'unsupported ProfileConfig schema' "$err"; then
    fail "schema error was not clear: $(cat "$err")"
    return 1
  fi
  pass "wrix run rejects unsupported ProfileConfig schema"
}

ALL_TESTS=(
  test_run_requires_profile_config
  test_run_rejects_invalid_profile_config_json
  test_run_rejects_wrong_profile_config_schema
)

run_all() {
  local fn rc
  for fn in "${ALL_TESTS[@]}"; do
    echo "=== $fn ==="
    rc=0
    "$fn" || rc=$?
    if [[ "$rc" -ne 0 && "$rc" -ne 77 ]]; then
      fail "$fn returned $rc without calling fail()"
    fi
  done
  echo
  echo "Results: $PASSED passed, $FAILED failed"
  [[ "$FAILED" -eq 0 ]]
}

if [[ $# -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    echo "Unknown function: $fn" >&2
    exit 1
  fi
  "$fn"
fi
