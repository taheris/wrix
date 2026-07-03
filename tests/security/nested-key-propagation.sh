#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

wrix_require_live_sandbox
cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-nested-key.XXXXXX)
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
IMAGE_REF=$(wrix_live_image_ref "nested-key-$$")
PROFILE_CONFIG="$TEST_TMP/profile.json"
SPAWN_CONFIG="$TEST_TMP/spawn.json"
WORKSPACE="$TEST_TMP/workspace"
HOME_DIR="$TEST_TMP/home"
XDG_CACHE_HOME="$TEST_TMP/cache"
HOST_KEY_DIR="$TEST_TMP/keys"
HOST_DEPLOY_KEY="$HOST_KEY_DIR/myrepo"
HOST_SIGNING_KEY="$HOST_KEY_DIR/myrepo-signing"
mkdir -p "$WORKSPACE" "$HOME_DIR" "$XDG_CACHE_HOME" "$HOST_KEY_DIR"
wrix_make_ed25519_key "$HOST_DEPLOY_KEY" "nested-key-test"
wrix_make_ed25519_key "$HOST_SIGNING_KEY" "nested-key-signing-test"
wrix_write_profile_config "$PROFILE_CONFIG" "$IMAGE_REF" "$IMAGE_SOURCE" claude
# shellcheck disable=SC2016
wrix_write_spawn_config "$SPAWN_CONFIG" "$WORKSPACE" bash -lc '
set -euo pipefail

fail_probe() {
  local message="$1"
  printf "nested-key probe failed: %s\n" "$message" >&2
  exit 1
}

case "${WRIX_DEPLOY_KEY:-}" in
  /etc/wrix/keys/*) ;;
  *) fail_probe "WRIX_DEPLOY_KEY is not staged under /etc/wrix/keys: ${WRIX_DEPLOY_KEY:-unset}" ;;
esac
case "${WRIX_SIGNING_KEY:-}" in
  /etc/wrix/keys/*-signing) ;;
  *) fail_probe "WRIX_SIGNING_KEY is not staged under /etc/wrix/keys: ${WRIX_SIGNING_KEY:-unset}" ;;
esac
[[ -f "$WRIX_DEPLOY_KEY" ]] || fail_probe "mounted deploy key is missing: $WRIX_DEPLOY_KEY"
[[ -f "$WRIX_SIGNING_KEY" ]] || fail_probe "mounted signing key is missing: $WRIX_SIGNING_KEY"
ssh_command=$(git config --global --get core.sshCommand)
case "$ssh_command" in
  *"-i $WRIX_DEPLOY_KEY"*"IdentitiesOnly=yes"*) ;;
  *) fail_probe "unexpected core.sshCommand: $ssh_command" ;;
esac
git init -q .
git commit --allow-empty -q -m test-signed
if ! git cat-file -p HEAD | grep -q "^gpgsig"; then
  fail_probe "signed commit is missing gpgsig metadata"
fi
'

test_nested_key_propagation() {
  local out="$TEST_TMP/nested-key.out"
  local err="$TEST_TMP/nested-key.err"
  local rc=0

  HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
    WRIX_DEPLOY_KEY="$HOST_DEPLOY_KEY" WRIX_SIGNING_KEY="$HOST_SIGNING_KEY" WRIX_GIT_SIGN=1 \
    wrix_run_spawn "$LAUNCHER" "$PROFILE_CONFIG" "$SPAWN_CONFIG" >"$out" 2>"$err" || rc=$?

  if [[ "$rc" -ne 0 ]]; then
    fail "live child container did not observe mounted keys or produce a signed commit"
    sed 's/^/    /' "$err" >&2
    return
  fi

  pass "live child container sees fixed key destinations and signs commits"
}

test_nested_key_propagation

echo
echo "Results: $PASSED passed, $FAILED failed"
[[ "$FAILED" -eq 0 ]]
