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
WRIX="$PACKAGE_LINK/bin/wrix"
WRIX_RUN="$PACKAGE_LINK/bin/wrix-run"
OPTIONAL_MOUNT_SOURCE="$TEST_TMP/missing-optional-mount"

build_wrapper_package() {
  local log="$TEST_TMP/package-build.log"
  if [[ -x "$WRIX" && -x "$WRIX_RUN" ]]; then
    return 0
  fi
  if ! nix build \
    --impure --no-warn-dirty \
    --out-link "$PACKAGE_LINK" \
    --expr "
      let
        flake = builtins.getFlake \"git+file://$REPO_ROOT\";
        system = builtins.currentSystem;
        lib = flake.legacyPackages.\${system}.lib;
        profile = lib.deriveProfile lib.profiles.base {
          mounts = [{
            source = \"$OPTIONAL_MOUNT_SOURCE\";
            dest = \"/mnt/optional-cache\";
            mode = \"rw\";
            optional = true;
          }];
        };
      in (lib.mkSandbox { inherit profile; }).package
    " >"$log" 2>&1; then
    fail "nix build of mkSandbox package failed"
    tail -n 80 "$log" >&2
    return 1
  fi
}

raw_wrix_launcher() {
  local link="$TEST_TMP/raw-wrix"
  local log="$TEST_TMP/raw-wrix-build.log"
  if [[ ! -x "$link/bin/wrix" ]]; then
    if ! nix build --no-warn-dirty --out-link "$link" "$REPO_ROOT#wrix" >"$log" 2>&1; then
      printf 'raw wrix launcher build failed\n' >&2
      tail -n 80 "$log" >&2
      return 1
    fi
  fi
  printf '%s\n' "$link/bin/wrix"
}

WORKSPACE="$TEST_TMP/workspace"
mkdir -p "$WORKSPACE"

profile_config_from_wrapper() {
  grep -oE -- '--profile-config[[:space:]]+[^[:space:]]+' "$WRIX" | awk '{print $2}' | head -1
}

test_wrapper_passes_store_profile_config_arg() {
  build_wrapper_package || return 1
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
  build_wrapper_package || return 1
  if grep -qE 'WRIX_DEFAULT_IMAGE_(REF|SOURCE|DIGEST)' "$WRIX"; then
    fail "wrapper still contains mutable WRIX_DEFAULT_IMAGE_* env defaults"
    return 1
  fi
  pass "wrapper does not set WRIX_DEFAULT_IMAGE_* env defaults"
}

test_profile_config_contains_launcher_contract_fields() {
  build_wrapper_package || return 1
  local config
  config=$(profile_config_from_wrapper)
  if ! jq -e \
    --arg source_kind "$EXPECTED_SOURCE_KIND" \
    --arg optional_source "$OPTIONAL_MOUNT_SOURCE" '
    .schema == 1 and
    (.system | type == "string") and
    (.profile.name | type == "string") and
    (.profile.env | type == "object") and
    (.profile.mounts == [{
      source: $optional_source,
      dest: "/mnt/optional-cache",
      mode: "rw",
      optional: true
    }]) and
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

test_missing_optional_profile_mount_is_skipped() {
  build_wrapper_package || return 1
  local out="$TEST_TMP/optional-mount.out" err="$TEST_TMP/optional-mount.err" rc=0
  if [[ -e "$OPTIONAL_MOUNT_SOURCE" ]]; then
    fail "optional mount fixture unexpectedly exists: $OPTIONAL_MOUNT_SOURCE"
    return 1
  fi

  WRIX_DRY_RUN=1 "$WRIX" run "$WORKSPACE" >"$out" 2>"$err" || rc=$?
  if [[ "$rc" != "0" ]]; then
    fail "missing optional profile mount failed launch planning: $(cat "$err")"
    return 1
  fi
  if grep -qF '/mnt/optional-cache' "$out"; then
    fail "missing optional profile mount reached container argv: $(cat "$out")"
    return 1
  fi
  pass "Nix-generated missing optional profile mount is skipped by the Rust launcher"
}

test_image_source_kind() {
  build_wrapper_package || return 1
  local config source kind missing_config bad_config bad_kind out err rc raw_wrix
  config=$(profile_config_from_wrapper)
  if ! raw_wrix=$(raw_wrix_launcher); then
    fail "raw wrix launcher is unavailable"
    return 1
  fi
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
  WRIX_DRY_RUN=1 "$raw_wrix" --profile-config "$missing_config" run "$WORKSPACE" >"$out" 2>"$err" || rc=$?
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
  WRIX_DRY_RUN=1 "$raw_wrix" --profile-config "$bad_config" run "$WORKSPACE" >"$out" 2>"$err" || rc=$?
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
  build_wrapper_package || return 1
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

write_deploy_key_fixture() {
  local output_path="$1"
  printf 'profile-config-wrapper deploy key fixture\n' >"$output_path"
}

test_wrapper_invokes_run_and_spawn_with_profile_config() {
  build_wrapper_package || return 1
  local run_out="$TEST_TMP/dry-run.out" run_err="$TEST_TMP/dry-run.err"
  local spawn_out="$TEST_TMP/dry-spawn.out" spawn_err="$TEST_TMP/dry-spawn.err"
  local main_out="$TEST_TMP/dry-main.out" main_err="$TEST_TMP/dry-main.err"
  local main_spawn_out="$TEST_TMP/dry-main-spawn.out" main_spawn_err="$TEST_TMP/dry-main-spawn.err"
  local bare_out="$TEST_TMP/bare-wrix.out" bare_err="$TEST_TMP/bare-wrix.err"
  local deploy_key="$TEST_TMP/deploy-key"
  local spawn_config="$TEST_TMP/spawn.json" run_config spawn_config_path rc=0

  if [[ ! -x "$WRIX_RUN" ]]; then
    fail "wrapped package does not expose executable wrix-run"
    return 1
  fi

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

  rc=0
  WRIX_DRY_RUN=1 "$WRIX" >"$bare_out" 2>"$bare_err" || rc=$?
  if [[ "$rc" != "0" ]]; then
    fail "bare wrix wrapper should print help successfully: $(cat "$bare_err")"
    return 1
  fi
  if grep -q '^SUBCOMMAND=' "$bare_out" || [[ "$(cat "$bare_out")" != *"Usage: wrix <command>"* ]]; then
    fail "bare wrix wrapper should remain explicit help, got: $(cat "$bare_out")"
    return 1
  fi

  rc=0
  WRIX_DRY_RUN=1 "$WRIX_RUN" "$WORKSPACE" >"$main_out" 2>"$main_err" || rc=$?
  if [[ "$rc" != "0" ]]; then
    fail "wrix-run default dry-run failed: $(cat "$main_err")"
    return 1
  fi
  if ! grep -qxF 'SUBCOMMAND=run' "$main_out"; then
    fail "wrix-run did not default to run: $(cat "$main_out")"
    return 1
  fi

  write_spawn_config "$spawn_config"
  write_deploy_key_fixture "$deploy_key"
  rc=0
  WRIX_DEPLOY_KEY="$deploy_key" WRIX_GIT_SIGN=0 WRIX_DRY_RUN=1 "$WRIX" spawn --spawn-config "$spawn_config" --stdio >"$spawn_out" 2>"$spawn_err" || rc=$?
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

  rc=0
  WRIX_DEPLOY_KEY="$deploy_key" WRIX_GIT_SIGN=0 WRIX_DRY_RUN=1 "$WRIX_RUN" spawn --spawn-config "$spawn_config" --stdio >"$main_spawn_out" 2>"$main_spawn_err" || rc=$?
  if [[ "$rc" != "0" ]]; then
    fail "wrix-run spawn dry-run failed: $(cat "$main_spawn_err")"
    return 1
  fi
  if ! grep -qxF 'SUBCOMMAND=spawn' "$main_spawn_out" || ! grep -qxF 'STDIO=1' "$main_spawn_out"; then
    fail "wrix-run did not pass explicit spawn through: $(cat "$main_spawn_out")"
    return 1
  fi

  pass "wrapped package exposes explicit wrix and runnable wrix-run with the same ProfileConfig"
}

WRAPPER_CONTRACT_TESTS=(
  test_wrapper_passes_store_profile_config_arg
  test_wrapper_does_not_set_image_default_env
  test_profile_config_contains_launcher_contract_fields
  test_wrapper_dispatches_service_commands
  test_wrapper_invokes_run_and_spawn_with_profile_config
)

ALL_TESTS=(
  "${WRAPPER_CONTRACT_TESTS[@]}"
  test_image_source_kind
  test_missing_optional_profile_mount_is_skipped
)

test_profile_config_wrapper_contract() {
  build_wrapper_package || return 1
  local fn failed=0
  for fn in "${WRAPPER_CONTRACT_TESTS[@]}"; do
    "$fn" || failed=$((failed + 1))
  done
  [[ "$failed" -eq 0 ]]
}

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
