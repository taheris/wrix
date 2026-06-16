#!/usr/bin/env bash
set -euo pipefail

if ! command -v nix >/dev/null 2>&1; then
  echo "skip: nix not on PATH" >&2
  exit 77
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "skip: jq not on PATH" >&2
  exit 77
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP=$(mktemp -d -t wrix-profile-config-agent-pin.XXXXXX)
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

PASSED=0
FAILED=0

pass() { printf '  PASS: %s\n' "$1"; PASSED=$((PASSED + 1)); }
fail() { printf '  FAIL: %s\n' "$1" >&2; FAILED=$((FAILED + 1)); }

PACKAGE_LINK="$TEST_TMP/package"
nix build \
  --impure --no-warn-dirty \
  --out-link "$PACKAGE_LINK" \
  --expr "
    let
      flake = builtins.getFlake \"git+file://$REPO_ROOT\";
      system = builtins.currentSystem;
      lib = flake.legacyPackages.\${system}.lib;
    in (lib.mkSandbox { profile = lib.profiles.base; }).package
  "

WRIX="$PACKAGE_LINK/bin/wrix"
WORKSPACE="$TEST_TMP/workspace"
mkdir -p "$WORKSPACE"

profile_config_from_wrapper() {
  grep -oE -- '--profile-config[[:space:]]+[^[:space:]]+' "$WRIX" | awk '{print $2}' | head -1
}

run_dry() {
  local out="$1"
  local err="$2"
  shift 2
  local rc=0
  WRIX_DRY_RUN=1 "$WRIX" "$@" >"$out" 2>"$err" || rc=$?
  echo "$rc"
}

test_profile_config_pins_agent_kind() {
  local config
  config=$(profile_config_from_wrapper)
  local kind
  kind=$(jq -r '.agent.kind' "$config")
  if [[ "$kind" != "direct" ]]; then
    fail "expected direct agent in test ProfileConfig, got $kind"
    return 1
  fi
  pass "ProfileConfig carries the selected agent kind"
}

test_caller_wrix_agent_env_cannot_override_profile_config() {
  local out="$TEST_TMP/env-override.out" err="$TEST_TMP/env-override.err" rc
  rc=$(WRIX_AGENT=pi run_dry "$out" "$err" run "$WORKSPACE")
  if [[ "$rc" != "0" ]]; then
    fail "dry-run with caller WRIX_AGENT failed: $(cat "$err")"
    return 1
  fi
  if ! grep -qxF 'PROFILE_AGENT=direct' "$out"; then
    fail "caller WRIX_AGENT changed launcher agent; output=$(cat "$out")"
    return 1
  fi
  pass "caller WRIX_AGENT env cannot change the ProfileConfig-selected agent"
}

test_spawn_config_agent_override_is_rejected() {
  local config="$TEST_TMP/spawn.json"
  cat > "$config" <<JSON
{
  "workspace": "$WORKSPACE",
  "image_ref": "wrix:test",
  "image_source": "",
  "env": [],
  "agent_args": [],
  "agent": { "kind": "pi" }
}
JSON

  local out="$TEST_TMP/spawn-agent.out" err="$TEST_TMP/spawn-agent.err" rc
  rc=$(run_dry "$out" "$err" spawn --spawn-config "$config")
  if [[ "$rc" == "0" ]]; then
    fail "SpawnConfig agent override was accepted"
    return 1
  fi
  if ! grep -qF 'SpawnConfig cannot change the ProfileConfig agent' "$err"; then
    fail "SpawnConfig agent override error was not clear: $(cat "$err")"
    return 1
  fi
  pass "SpawnConfig cannot change the selected agent independently"
}

test_wrapper_has_no_agent_env_setter() {
  if grep -q 'WRIX_AGENT=' "$WRIX"; then
    fail "wrapper still sets WRIX_AGENT env instead of relying on ProfileConfig"
    return 1
  fi
  pass "wrapper does not set WRIX_AGENT env"
}

ALL_TESTS=(
  test_profile_config_pins_agent_kind
  test_caller_wrix_agent_env_cannot_override_profile_config
  test_spawn_config_agent_override_is_rejected
  test_wrapper_has_no_agent_env_setter
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
