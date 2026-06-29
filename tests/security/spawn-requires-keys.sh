#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

wrix_require_live_sandbox
command -v script >/dev/null 2>&1 || wrix_live_skip "script not on PATH"
cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-spawn-keys.XXXXXX)
cleanup() {
  rm -rf "$TEST_TMP"
  wrix_remove_image_ref "${IMAGE_REF:-}"
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

LAUNCHER=$(wrix_build_live_launcher)
IMAGE_SOURCE=$(wrix_realize_test_image_source claude)
IMAGE_REF=$(wrix_live_image_ref "spawn-keys-$$")
PROFILE_CONFIG="$TEST_TMP/profile.json"
HOME_DIR="$TEST_TMP/home"
XDG_CACHE_HOME="$TEST_TMP/cache"
DEPLOY_KEY="$TEST_TMP/deploy-key"
mkdir -p "$HOME_DIR/.ssh/deploy_keys" "$XDG_CACHE_HOME"
wrix_make_ed25519_key "$DEPLOY_KEY" "spawn-key-test"
wrix_write_profile_config "$PROFILE_CONFIG" "$IMAGE_REF" "$IMAGE_SOURCE" claude

write_case_spawn_config() {
  local label="$1"
  local workspace="$TEST_TMP/workspace-$label"
  local spawn_config="$TEST_TMP/$label.spawn.json"
  shift

  mkdir -p "$workspace"
  wrix_write_spawn_config "$spawn_config" "$workspace" "$@"
  printf '%s\n' "$spawn_config"
}

assert_spawn_fails_no_keys() {
  local label="spawn-no-keys"
  local spawn_config out err rc

  spawn_config=$(write_case_spawn_config "$label" bash -lc 'exit 0')
  out="$TEST_TMP/$label.out"
  err="$TEST_TMP/$label.err"
  rc=0
  env -u WRIX_DEPLOY_KEY -u WRIX_SIGNING_KEY -u WRIX_GIT_SIGN \
    HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
    "$LAUNCHER/bin/wrix" --profile-config "$PROFILE_CONFIG" spawn --spawn-config "$spawn_config" \
    >"$out" 2>"$err" || rc=$?

  if [[ "$rc" -eq 0 ]]; then
    fail "$label: wrix spawn exited 0 without keys"
    return
  fi
  if ! grep -qF "no deploy key resolved" "$err"; then
    fail "$label: stderr does not name unresolved deploy key"
    sed 's/^/    /' "$err" >&2
    return
  fi
  pass "$label: live wrix spawn fails loud before container start"
}

assert_spawn_fails_no_signing() {
  local label="spawn-no-signing"
  local spawn_config out err rc

  spawn_config=$(write_case_spawn_config "$label" bash -lc 'exit 0')
  out="$TEST_TMP/$label.out"
  err="$TEST_TMP/$label.err"
  rc=0
  env -u WRIX_SIGNING_KEY -u WRIX_GIT_SIGN \
    HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" WRIX_DEPLOY_KEY="$DEPLOY_KEY" \
    "$LAUNCHER/bin/wrix" --profile-config "$PROFILE_CONFIG" spawn --spawn-config "$spawn_config" \
    >"$out" 2>"$err" || rc=$?

  if [[ "$rc" -eq 0 ]]; then
    fail "$label: wrix spawn exited 0 with no signing key"
    return
  fi
  if ! grep -qF "no signing key resolved" "$err"; then
    fail "$label: stderr does not name unresolved signing key"
    sed 's/^/    /' "$err" >&2
    return
  fi
  pass "$label: live wrix spawn fails loud on missing signing key"
}

assert_spawn_nosign_deploy_only() {
  local label="spawn-nosign-deploy"
  local spawn_config out err rc

  # shellcheck disable=SC2016
  spawn_config=$(write_case_spawn_config "$label" bash -lc '[[ -f "${WRIX_DEPLOY_KEY:?}" ]] && [[ -z "${WRIX_SIGNING_KEY:-}" ]]')
  out="$TEST_TMP/$label.out"
  err="$TEST_TMP/$label.err"
  rc=0
  env -u WRIX_SIGNING_KEY \
    HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_GIT_SIGN=0 \
    "$LAUNCHER/bin/wrix" --profile-config "$PROFILE_CONFIG" spawn --spawn-config "$spawn_config" \
    >"$out" 2>"$err" || rc=$?

  if [[ "$rc" -ne 0 ]]; then
    fail "$label: wrix spawn with WRIX_GIT_SIGN=0 and a deploy key failed"
    sed 's/^/    /' "$err" >&2
    return
  fi
  pass "$label: WRIX_GIT_SIGN=0 permits deploy-only spawn and starts container"
}

assert_spawn_nosign_still_needs_deploy() {
  local label="spawn-nosign-no-deploy"
  local spawn_config out err rc

  spawn_config=$(write_case_spawn_config "$label" bash -lc 'exit 0')
  out="$TEST_TMP/$label.out"
  err="$TEST_TMP/$label.err"
  rc=0
  env -u WRIX_DEPLOY_KEY -u WRIX_SIGNING_KEY \
    HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" WRIX_GIT_SIGN=0 \
    "$LAUNCHER/bin/wrix" --profile-config "$PROFILE_CONFIG" spawn --spawn-config "$spawn_config" \
    >"$out" 2>"$err" || rc=$?

  if [[ "$rc" -eq 0 ]]; then
    fail "$label: wrix spawn exited 0 without a deploy key"
    return
  fi
  if ! grep -qF "no deploy key resolved" "$err"; then
    fail "$label: stderr does not name unresolved deploy key"
    sed 's/^/    /' "$err" >&2
    return
  fi
  pass "$label: WRIX_GIT_SIGN=0 still requires deploy key"
}

assert_run_permits_no_keys() {
  local label="run-no-keys"
  local workspace="$TEST_TMP/workspace-$label"
  local out="$TEST_TMP/$label.out"
  local err="$TEST_TMP/$label.err"
  local rc=0
  local command_line=""
  local -a cmd

  mkdir -p "$workspace"
  # shellcheck disable=SC2016
  cmd=(
    "$LAUNCHER/bin/wrix" --profile-config "$PROFILE_CONFIG" run "$workspace"
    bash -lc '[[ -z "${WRIX_DEPLOY_KEY:-}" && -z "${WRIX_SIGNING_KEY:-}" ]]'
  )
  printf -v command_line '%q ' "${cmd[@]}"
  (
    unset WRIX_DEPLOY_KEY WRIX_SIGNING_KEY WRIX_GIT_SIGN
    export HOME="$HOME_DIR"
    export XDG_CACHE_HOME="$XDG_CACHE_HOME"
    wrix_run_with_pty "$command_line"
  ) >"$out" 2>"$err" || rc=$?

  if [[ "$rc" -ne 0 ]]; then
    fail "$label: wrix run did not boot keyless"
    sed 's/^/    /' "$err" >&2
    return
  fi
  pass "$label: live wrix run boots without keys"
}

assert_spawn_fails_no_keys
assert_spawn_fails_no_signing
assert_spawn_nosign_deploy_only
assert_spawn_nosign_still_needs_deploy
assert_run_permits_no_keys

echo
echo "Results: $PASSED passed, $FAILED failed"
[[ "$FAILED" -eq 0 ]]
