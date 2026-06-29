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

expected_source_kind() {
  case "$(uname -s)" in
    Darwin) printf 'docker-archive\n' ;;
    *) printf 'nix-descriptor\n' ;;
  esac
}

alternate_source_kind() {
  case "$EXPECTED_SOURCE_KIND" in
    docker-archive) printf 'nix-descriptor\n' ;;
    *) printf 'docker-archive\n' ;;
  esac
}

EXPECTED_SOURCE_KIND=$(expected_source_kind)

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
  if ! jq -e --arg source_kind "$EXPECTED_SOURCE_KIND" '
    .schema == 1 and
    (.system | type == "string") and
    (.profile.name | type == "string") and
    (.profile.env | type == "object") and
    (.profile.mounts | type == "array") and
    (.profile.writable_dirs | type == "array") and
    (.profile.network_allowlist | type == "array") and
    (.image.ref | type == "string" and length > 0) and
    (.image.source | type == "string" and length > 0) and
    (.image.source_kind == $source_kind) and
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

test_image_source_kind() {
  local config source kind missing_config bad_config bad_kind out err rc
  config=$(profile_config_from_wrapper)
  source=$(jq -r '.image.source // ""' "$config")
  kind=$(jq -r '.image.source_kind // ""' "$config")
  if [[ "$kind" != "$EXPECTED_SOURCE_KIND" ]]; then
    fail "ProfileConfig image.source_kind=$kind, expected $EXPECTED_SOURCE_KIND"
    return 1
  fi
  if [[ "$source" != /nix/store/* ]]; then
    fail "ProfileConfig image.source is not store-resident: $source"
    return 1
  fi

  missing_config="$TEST_TMP/missing-source-kind.json"
  jq 'del(.image.source_kind)' "$config" >"$missing_config"
  out="$TEST_TMP/missing-source-kind.out"
  err="$TEST_TMP/missing-source-kind.err"
  rc=0
  WRIX_DRY_RUN=1 "$WRIX" --profile-config "$missing_config" run "$WORKSPACE" >"$out" 2>"$err" || rc=$?
  if [[ "$rc" == "0" ]]; then
    fail "launcher accepted ProfileConfig without image.source_kind"
    return 1
  fi
  if ! grep -qF 'image.source_kind' "$err"; then
    fail "missing source_kind error did not name image.source_kind: $(cat "$err")"
    return 1
  fi

  bad_kind=$(alternate_source_kind)
  bad_config="$TEST_TMP/bad-source-kind.json"
  jq --arg kind "$bad_kind" '.image.source_kind = $kind' "$config" >"$bad_config"
  out="$TEST_TMP/bad-source-kind.out"
  err="$TEST_TMP/bad-source-kind.err"
  rc=0
  WRIX_DRY_RUN=1 "$WRIX" --profile-config "$bad_config" run "$WORKSPACE" >"$out" 2>"$err" || rc=$?
  if [[ "$rc" == "0" ]]; then
    fail "launcher accepted incompatible ProfileConfig image.source_kind=$bad_kind"
    return 1
  fi
  if ! grep -qF "image.source_kind must be $EXPECTED_SOURCE_KIND" "$err"; then
    fail "bad source_kind error did not name expected kind: $(cat "$err")"
    return 1
  fi

  pass "ProfileConfig carries platform source_kind and launcher rejects missing/incompatible kinds"
}

test_wrapper_dispatches_service_commands() {
  local output
  output="$($WRIX service --help)"
  if [[ "$output" != *"Usage: wrix service <command>"* ]]; then
    fail "wrapped package did not dispatch service commands to the Rust CLI: $output"
    return 1
  fi
  pass "wrapped package dispatches service commands to the Rust CLI"
}

profile_config_from_dry_run_output() {
  local output_path="$1"
  awk -F= '$1 == "PROFILE_CONFIG" { print $2; exit }' "$output_path"
}

write_spawn_config() {
  local output_path="$1"
  jq -n --arg workspace "$WORKSPACE" '{ workspace: $workspace, env: [], agent_args: ["true"], mounts: [] }' >"$output_path"
}

test_wrapper_invokes_run_and_spawn_with_profile_config() {
  local run_out="$TEST_TMP/dry-run.out" run_err="$TEST_TMP/dry-run.err"
  local spawn_out="$TEST_TMP/dry-spawn.out" spawn_err="$TEST_TMP/dry-spawn.err"
  local spawn_config="$TEST_TMP/spawn.json" run_config spawn_config_path rc=0

  WRIX_DRY_RUN=1 "$WRIX" run "$WORKSPACE" >"$run_out" 2>"$run_err" || rc=$?
  if [[ "$rc" != "0" ]]; then
    fail "wrapped run dry-run failed: $(cat "$run_err")"
    return 1
  fi
  run_config=$(profile_config_from_dry_run_output "$run_out")
  if [[ "$run_config" != /nix/store/* || ! -f "$run_config" ]]; then
    fail "run dry-run did not expose a store ProfileConfig path: $(cat "$run_out")"
    return 1
  fi
  if ! grep -qxF 'SUBCOMMAND=run' "$run_out"; then
    fail "run dry-run did not reach the launcher run parser: $(cat "$run_out")"
    return 1
  fi

  write_spawn_config "$spawn_config"
  rc=0
  WRIX_DRY_RUN=1 "$WRIX" spawn --spawn-config "$spawn_config" --stdio >"$spawn_out" 2>"$spawn_err" || rc=$?
  if [[ "$rc" != "0" ]]; then
    fail "wrapped spawn dry-run failed: $(cat "$spawn_err")"
    return 1
  fi
  spawn_config_path=$(profile_config_from_dry_run_output "$spawn_out")
  if [[ "$spawn_config_path" != "$run_config" ]]; then
    fail "spawn did not receive the same wrapper ProfileConfig; run=$run_config spawn=$spawn_config_path output=$(cat "$spawn_out")"
    return 1
  fi
  if ! grep -qxF 'SUBCOMMAND=spawn' "$spawn_out" || ! grep -qxF 'STDIO=1' "$spawn_out"; then
    fail "spawn dry-run did not reach the launcher spawn parser: $(cat "$spawn_out")"
    return 1
  fi
  pass "wrapped package passes a store ProfileConfig through live run and spawn parsing"
}

ALL_TESTS=(
  test_wrapper_passes_store_profile_config_arg
  test_wrapper_does_not_set_image_default_env
  test_profile_config_contains_launcher_contract_fields
  test_image_source_kind
  test_wrapper_dispatches_service_commands
  test_wrapper_invokes_run_and_spawn_with_profile_config
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
