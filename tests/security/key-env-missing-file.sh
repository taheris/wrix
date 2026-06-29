#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

wrix_require_live_sandbox
cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-key-missing.XXXXXX)
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
IMAGE_REF=$(wrix_live_image_ref "key-missing-$$")
PROFILE_CONFIG="$TEST_TMP/profile.json"
SPAWN_CONFIG="$TEST_TMP/spawn.json"
WORKSPACE="$TEST_TMP/workspace"
HOME_DIR="$TEST_TMP/home"
XDG_CACHE_HOME="$TEST_TMP/cache"
MISSING_PATH="$TEST_TMP/does-not-exist/key"
DEPLOY_KEY="$TEST_TMP/deploy-key"
mkdir -p "$WORKSPACE" "$HOME_DIR" "$XDG_CACHE_HOME" "$(dirname "$MISSING_PATH")"
printf 'fake deploy key\n' >"$DEPLOY_KEY"
wrix_write_profile_config "$PROFILE_CONFIG" "$IMAGE_REF" "$IMAGE_SOURCE" claude
wrix_write_spawn_config "$SPAWN_CONFIG" "$WORKSPACE" bash -lc 'exit 0'

assert_fails_with_path() {
  local label="$1"
  local env_var="$2"
  local out="$TEST_TMP/$label.out"
  local err="$TEST_TMP/$label.err"
  local rc=0

  case "$env_var" in
    WRIX_DEPLOY_KEY)
      HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
        WRIX_DEPLOY_KEY="$MISSING_PATH" \
        wrix_run_spawn "$LAUNCHER" "$PROFILE_CONFIG" "$SPAWN_CONFIG" >"$out" 2>"$err" || rc=$?
      ;;
    WRIX_SIGNING_KEY)
      HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
        WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_SIGNING_KEY="$MISSING_PATH" WRIX_GIT_SIGN=1 \
        wrix_run_spawn "$LAUNCHER" "$PROFILE_CONFIG" "$SPAWN_CONFIG" >"$out" 2>"$err" || rc=$?
      ;;
    *)
      fail "$label: unsupported env var $env_var"
      return
      ;;
  esac

  if [[ "$rc" -eq 0 ]]; then
    fail "$label: launcher exited 0 with $env_var=$MISSING_PATH"
    return
  fi
  if ! grep -qF "$env_var=$MISSING_PATH" "$err"; then
    fail "$label: stderr does not name $env_var=$MISSING_PATH"
    sed 's/^/    /' "$err" >&2
    return
  fi
  if ! grep -qF "file does not exist" "$err"; then
    fail "$label: stderr does not say file does not exist"
    sed 's/^/    /' "$err" >&2
    return
  fi
  if [[ -d "$WORKSPACE/.wrix/log" ]]; then
    fail "$label: session log exists, so the container reached entrypoint startup"
    return
  fi

  pass "$label: live launcher fails before container start and names the missing path"
}

assert_fails_with_path deploy-env-missing WRIX_DEPLOY_KEY
assert_fails_with_path signing-env-missing WRIX_SIGNING_KEY

echo
echo "Results: $PASSED passed, $FAILED failed"
[[ "$FAILED" -eq 0 ]]
