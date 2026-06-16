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
TEST_TMP=$(mktemp -d -t wrix-profile-config-wrapper.XXXXXX)
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

test_wrapper_passes_store_profile_config_arg() {
  local config
  config=$(profile_config_from_wrapper)
  if [[ -z "$config" ]]; then
    fail "wrapper does not pass --profile-config"
    return 1
  fi
  if [[ "$config" != /nix/store/* ]]; then
    fail "ProfileConfig is not store-resident: $config"
    return 1
  fi
  if [[ ! -f "$config" ]]; then
    fail "ProfileConfig path is missing: $config"
    return 1
  fi
  pass "wrapper passes a store-resident ProfileConfig path"
}

test_wrapper_does_not_set_image_default_env() {
  if grep -qE 'WRIX_DEFAULT_IMAGE_(REF|SOURCE|DIGEST)' "$WRIX"; then
    fail "wrapper still contains mutable WRIX_DEFAULT_IMAGE_* env defaults"
    return 1
  fi
  pass "wrapper does not set WRIX_DEFAULT_IMAGE_* env defaults"
}

test_profile_config_contains_launcher_contract_fields() {
  local config
  config=$(profile_config_from_wrapper)
  if ! jq -e '
    .schema == 1 and
    (.system | type == "string") and
    (.profile.name | type == "string") and
    (.profile.env | type == "object") and
    (.profile.mounts | type == "array") and
    (.profile.writable_dirs | type == "array") and
    (.profile.network_allowlist | type == "array") and
    (.image.ref | type == "string" and length > 0) and
    (.image.source | type == "string" and length > 0) and
    (.image.digest | type == "string" and startswith("sha256:")) and
    (.agent.kind == "direct") and
    (.resources.memory_mb | type == "number") and
    (.resources.pids_limit | type == "number") and
    (.security | type == "object") and
    (.network.default_mode == "open") and
    (.network.ipv6 == "disabled") and
    (.services.beads.enable == "auto") and
    (.services.nix_cache.enable == true) and
    (.features.mcp_runtime == false)
  ' "$config" >/dev/null; then
    fail "ProfileConfig is missing required contract fields: $(cat "$config")"
    return 1
  fi
  pass "ProfileConfig JSON contains the schema v1 launcher contract fields"
}

test_wrapper_invokes_launcher_with_profile_config() {
  local out="$TEST_TMP/dry-run.out" err="$TEST_TMP/dry-run.err" config rc=0
  config=$(profile_config_from_wrapper)
  WRIX_DRY_RUN=1 "$WRIX" run "$WORKSPACE" >"$out" 2>"$err" || rc=$?
  if [[ "$rc" != "0" ]]; then
    fail "wrapped dry-run failed: $(cat "$err")"
    return 1
  fi
  if ! grep -qxF "PROFILE_CONFIG=$config" "$out"; then
    fail "launcher did not receive wrapper ProfileConfig; output=$(cat "$out")"
    return 1
  fi
  pass "wrapped package passes --profile-config through to the launcher"
}

ALL_TESTS=(
  test_wrapper_passes_store_profile_config_arg
  test_wrapper_does_not_set_image_default_env
  test_profile_config_contains_launcher_contract_fields
  test_wrapper_invokes_launcher_with_profile_config
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
