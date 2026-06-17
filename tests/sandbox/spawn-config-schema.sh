#!/usr/bin/env bash
set -euo pipefail

if ! command -v nix >/dev/null; then
  echo "skip: nix not on PATH" >&2
  exit 77
fi
if ! command -v jq >/dev/null; then
  echo "skip: jq not on PATH" >&2
  exit 77
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-spawn-schema.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

PASSED=0
FAILED=0

pass() {
  local message="$1"
  printf '  PASS: %s\n' "$message"
  PASSED=$((PASSED + 1))
}

fail() {
  local message="$1"
  printf '  FAIL: %s\n' "$message" >&2
  FAILED=$((FAILED + 1))
}

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
if [[ ! -x "$WRIX" ]]; then
  echo "fixture error: launcher binary not built at $WRIX" >&2
  exit 1
fi

PROFILE_CONFIG="$TEST_TMP/profile-config.json"
cat >"$PROFILE_CONFIG" <<'JSON'
{"schema":1,"system":"test","profile":{"name":"base","env":{},"mounts":[],"writable_dirs":[],"network_allowlist":[]},"image":{"ref":"wrix-base:test","source":"/nix/store/fake-image","digest":"sha256:test"},"agent":{"kind":"direct"},"resources":{"cpus":null,"memory_mb":4096,"pids_limit":4096},"security":{"deploy_key":null},"network":{"default_mode":"open","ipv6":"disabled"},"services":{"beads":{"enable":"auto"},"nix_cache":{"enable":true}},"features":{"mcp_runtime":false}}
JSON

WORKSPACE="$TEST_TMP/workspace"
mkdir -p "$WORKSPACE"

write_valid_spawn_config() {
  local out_file="$1"
  local mount_host="$2"
  jq -n \
    --arg workspace "$WORKSPACE" \
    --arg mount_host "$mount_host" \
    '{
      workspace: $workspace,
      image_ref: "wrix-override:test",
      image_source: "/nix/store/fake-override",
      env: [["FOO", "bar"], ["EMPTY", ""]],
      agent_args: ["--print", "hello"],
      mounts: [{host_path: $mount_host, container_path: "/mnt/schema", read_only: true}],
      initial_prompt: "ignored by launcher",
      repin: true
    }' >"$out_file"
}

run_launcher() {
  local out_file="$1"
  local err_file="$2"
  shift 2
  local rc=0
  WRIX_DRY_RUN=1 "$WRIX" "$@" >"$out_file" 2>"$err_file" || rc=$?
  printf '%s\n' "$rc"
}

assert_file_contains() {
  local label="$1"
  local file="$2"
  local needle="$3"
  if ! grep -qF -- "$needle" "$file"; then
    fail "$label: missing '$needle' in $(cat "$file")"
    return 1
  fi
}

test_spawn_requires_profile_config() {
  local mount_host="$TEST_TMP/required-mount"
  mkdir -p "$mount_host"
  local config="$TEST_TMP/requires-profile.json"
  write_valid_spawn_config "$config" "$mount_host"

  local out="$TEST_TMP/requires-profile.out" err="$TEST_TMP/requires-profile.err" rc
  rc=$(run_launcher "$out" "$err" spawn --spawn-config "$config")
  if [[ "$rc" == "0" ]]; then
    fail "spawn without --profile-config exited 0"
    return 1
  fi
  assert_file_contains "missing profile config" "$err" "--profile-config" || return 1
  assert_file_contains "missing profile config" "$err" "ProfileConfig" || return 1
  pass "wrix spawn requires the same ProfileConfig input as run"
}

test_documented_spawn_fields_parse() {
  local mount_host="$TEST_TMP/documented-mount"
  mkdir -p "$mount_host"
  local config="$TEST_TMP/documented.json"
  write_valid_spawn_config "$config" "$mount_host"

  local out="$TEST_TMP/documented.out" err="$TEST_TMP/documented.err" rc
  rc=$(run_launcher "$out" "$err" --profile-config "$PROFILE_CONFIG" spawn --spawn-config "$config" --stdio)
  if [[ "$rc" != "0" ]]; then
    fail "documented fields: launcher exited $rc; stderr=$(cat "$err")"
    return 1
  fi

  assert_file_contains "documented fields" "$out" "STDIO=1" || return 1
  assert_file_contains "documented fields" "$out" "PROFILE_CONFIG=$PROFILE_CONFIG" || return 1
  assert_file_contains "documented fields" "$out" "PROFILE_AGENT=direct" || return 1
  assert_file_contains "documented fields" "$out" "WORKSPACE=$WORKSPACE" || return 1
  assert_file_contains "documented fields" "$out" "IMAGE_OVERRIDE_REF=wrix-override:test" || return 1
  assert_file_contains "documented fields" "$out" "IMAGE_OVERRIDE_SOURCE=/nix/store/fake-override" || return 1
  assert_file_contains "documented fields" "$out" "ENV=FOO=bar" || return 1
  assert_file_contains "documented fields" "$out" "ENV=EMPTY=" || return 1
  assert_file_contains "documented fields" "$out" "CMD=--print" || return 1
  assert_file_contains "documented fields" "$out" "CMD=hello" || return 1
  assert_file_contains "documented fields" "$out" "$mount_host" || return 1
  assert_file_contains "documented fields" "$out" "/mnt/schema" || return 1
  pass "documented SpawnConfig fields parse while consumer-defined fields are ignored"
}

test_spawn_rejects_bad_schema_types() {
  local config="$TEST_TMP/bad-schema.json"
  jq -n '{workspace: 7, image_ref: "wrix:test", image_source: "", env: [], agent_args: [], mounts: []}' >"$config"

  local out="$TEST_TMP/bad-schema.out" err="$TEST_TMP/bad-schema.err" rc
  rc=$(run_launcher "$out" "$err" --profile-config "$PROFILE_CONFIG" spawn --spawn-config "$config")
  if [[ "$rc" == "0" ]]; then
    fail "bad schema types were accepted"
    return 1
  fi
  assert_file_contains "bad schema" "$err" "invalid SpawnConfig schema" || return 1
  assert_file_contains "bad schema" "$err" "workspace string" || return 1
  pass "SpawnConfig rejects invalid documented-field types"
}

test_spawn_rejects_profile_override_fields() {
  local field config out err rc
  for field in agent profile image_agent; do
    config="$TEST_TMP/override-$field.json"
    case "$field" in
      agent)
        jq -n --arg workspace "$WORKSPACE" '{workspace: $workspace, image_ref: "wrix:test", image_source: "", env: [], agent_args: [], mounts: [], agent: {kind: "pi"}}' >"$config"
        ;;
      profile)
        jq -n --arg workspace "$WORKSPACE" '{workspace: $workspace, image_ref: "wrix:test", image_source: "", env: [], agent_args: [], mounts: [], profile: {name: "python"}}' >"$config"
        ;;
      image_agent)
        jq -n --arg workspace "$WORKSPACE" '{workspace: $workspace, image_ref: "wrix:test", image_source: "", env: [], agent_args: [], mounts: [], image_agent: "pi"}' >"$config"
        ;;
    esac

    out="$TEST_TMP/override-$field.out"
    err="$TEST_TMP/override-$field.err"
    rc=$(run_launcher "$out" "$err" --profile-config "$PROFILE_CONFIG" spawn --spawn-config "$config")
    if [[ "$rc" == "0" ]]; then
      fail "ProfileConfig override field $field was accepted"
      return 1
    fi
    assert_file_contains "override $field" "$err" "cannot change the ProfileConfig agent/profile/image-agent field: $field" || return 1
  done
  pass "SpawnConfig rejects agent/profile/image-agent overrides"
}

test_spawn_rejects_undocumented_digest_override() {
  local config="$TEST_TMP/digest.json"
  jq -n --arg workspace "$WORKSPACE" '{workspace: $workspace, image_ref: "wrix:test", image_source: "", env: [], agent_args: [], mounts: [], image_digest_path: "/tmp/digest"}' >"$config"

  local out="$TEST_TMP/digest.out" err="$TEST_TMP/digest.err" rc
  rc=$(run_launcher "$out" "$err" --profile-config "$PROFILE_CONFIG" spawn --spawn-config "$config")
  if [[ "$rc" == "0" ]]; then
    fail "undocumented image_digest_path override was accepted"
    return 1
  fi
  assert_file_contains "digest override" "$err" "image_digest_path" || return 1
  assert_file_contains "digest override" "$err" "not a documented per-launch override" || return 1
  pass "SpawnConfig rejects undocumented image_digest_path override"
}

test_spawn_rejects_invalid_mount_entry() {
  local config="$TEST_TMP/bad-mount.json"
  jq -n --arg workspace "$WORKSPACE" --arg mount_host "$TEST_TMP/mount-without-read-only" '{workspace: $workspace, image_ref: "wrix:test", image_source: "", env: [], agent_args: [], mounts: [{host_path: $mount_host, container_path: "/mnt/bad"}]}' >"$config"

  local out="$TEST_TMP/bad-mount.out" err="$TEST_TMP/bad-mount.err" rc
  rc=$(run_launcher "$out" "$err" --profile-config "$PROFILE_CONFIG" spawn --spawn-config "$config")
  if [[ "$rc" == "0" ]]; then
    fail "invalid mount entry was accepted"
    return 1
  fi
  assert_file_contains "bad mount" "$err" "invalid SpawnConfig schema" || return 1
  assert_file_contains "bad mount" "$err" "host_path/container_path/read_only" || return 1
  pass "SpawnConfig validates mount entry shape before rendering mounts"
}

ALL_TESTS=(
  test_spawn_requires_profile_config
  test_documented_spawn_fields_parse
  test_spawn_rejects_bad_schema_types
  test_spawn_rejects_profile_override_fields
  test_spawn_rejects_undocumented_digest_override
  test_spawn_rejects_invalid_mount_entry
)

run_all() {
  local fn rc
  for fn in "${ALL_TESTS[@]}"; do
    printf '=== %s ===\n' "$fn"
    rc=0
    "$fn" || rc=$?
    if [[ "$rc" -ne 0 && "$rc" -ne 77 ]]; then
      fail "$fn returned $rc without completing"
    fi
  done
  printf '\nResults: %s passed, %s failed\n' "$PASSED" "$FAILED"
  [[ "$FAILED" -eq 0 ]]
}

if [[ "$#" -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null; then
    echo "Unknown function: $fn" >&2
    exit 1
  fi
  "$fn"
fi
