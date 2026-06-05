#!/usr/bin/env bash
# Verifier for criterion 202 of specs/security.md:
#
#   When WRIX_DEPLOY_KEY or WRIX_SIGNING_KEY is set in the launcher's
#   environment but the pointed-at file does not exist, the launcher exits
#   non-zero with a stderr message naming the missing path, before the
#   container is started.
#
# The launcher's resolution block is extracted from the writeShellApplication
# `text = ''…''` body in lib/sandbox/{linux,darwin}/default.nix, the
# `''$` Nix escape is converted back to `$`, and the Nix interpolations
# `${deployKeyExpr}` / `${sshConfig.containerKeyDir}` are substituted with
# concrete values for the test. We assert the block exits non-zero with the
# missing path in stderr when either env var points at a non-existent file.
#
# A separate "no container started" assertion is implicit: the block under
# test invokes no container runtime — the launcher's `exec podman run` /
# `container run` call site is downstream of this block, so a non-zero
# exit before that call site is a proof that no container starts.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
LINUX_LAUNCHER_NIX="$REPO_ROOT/lib/sandbox/linux/default.nix"
DARWIN_LAUNCHER_NIX="$REPO_ROOT/lib/sandbox/darwin/default.nix"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

for tool in awk sed; do
  command -v "$tool" >/dev/null 2>&1 || skip "$tool not on PATH"
done

TEST_TMP=$(mktemp -d -t wrix-key-missing.XXXXXX)
trap 'rm -rf "$TEST_TMP"' EXIT

PASSED=0
FAILED=0
pass() {
  printf '  PASS: %s\n' "$1"
  PASSED=$((PASSED + 1))
}
fail() {
  printf '  FAIL: %s\n' "$1" >&2
  FAILED=$((FAILED + 1))
}

extract_keyresolve_block() {
  local source="$1" out="$2"
  # SC2016: single quotes are intentional — the `${…}` patterns are
  # Nix interpolation tokens we want sed to match literally.
  # shellcheck disable=SC2016
  awk '
    /deploy key and signing key/ { capture = 1 }
    /\$\{stageBeads\}/           { capture = 0 }
    capture                       { print }
  ' "$source" \
    | sed -e 's|\${deployKeyExpr}|"myrepo"|g' \
          -e 's|\${sshConfig\.containerKeyDir}|/etc/wrix/keys|g' \
          -e "s/''\\\$/\$/g" \
    > "$out"
}

EMPTY_HOME="$TEST_TMP/empty-home"
mkdir -p "$EMPTY_HOME/.ssh/deploy_keys"
MISSING_PATH="$TEST_TMP/does-not-exist/key"

assert_fails_with_path() {
  local label="$1" platform_block="$2" env_var="$3"
  local probe="$TEST_TMP/$label.sh"
  cat >"$probe" <<EOF
set -e
HOME=$EMPTY_HOME
VOLUME_ARGS=""
MOUNT_ARGS=""
FILE_MOUNTS=""
DEPLOY_KEY_ARGS=""
STAGING_ROOT=$TEST_TMP/staging-$label
mkdir -p "\$STAGING_ROOT"
$env_var=$MISSING_PATH
source $platform_block
echo "REACHED_END_UNEXPECTEDLY"
EOF

  local err="$TEST_TMP/$label.err"
  local out="$TEST_TMP/$label.out"
  if bash "$probe" >"$out" 2>"$err"; then
    fail "$label: block exited 0 with $env_var=$MISSING_PATH (expected non-zero)"
    return
  fi
  if grep -qF "REACHED_END_UNEXPECTEDLY" "$out"; then
    fail "$label: block fell through past the missing-file check"
    return
  fi
  if ! grep -qF "$env_var=$MISSING_PATH" "$err"; then
    fail "$label: stderr does not name the missing path. stderr: $(cat "$err")"
    return
  fi
  if ! grep -qF "file does not exist" "$err"; then
    fail "$label: stderr does not say 'file does not exist'. stderr: $(cat "$err")"
    return
  fi
  pass "$label: fail-loud with stderr naming $env_var=$MISSING_PATH"
}

linux_block="$TEST_TMP/linux.sh"
darwin_block="$TEST_TMP/darwin.sh"
extract_keyresolve_block "$LINUX_LAUNCHER_NIX" "$linux_block"
extract_keyresolve_block "$DARWIN_LAUNCHER_NIX" "$darwin_block"

assert_fails_with_path linux-deploy-missing "$linux_block" WRIX_DEPLOY_KEY
assert_fails_with_path linux-signing-missing "$linux_block" WRIX_SIGNING_KEY
assert_fails_with_path darwin-deploy-missing "$darwin_block" WRIX_DEPLOY_KEY
assert_fails_with_path darwin-signing-missing "$darwin_block" WRIX_SIGNING_KEY

# ----------------------------------------------------------------------------
# Sanity check: when env is unset, the block falls through to the $HOME
# fallback (which is absent in EMPTY_HOME), so the block exits cleanly with
# no key wired in. This rules out a regression where the fail-loud branch
# fires on env-unset.
# ----------------------------------------------------------------------------
test_no_env_no_fail() {
  local probe="$TEST_TMP/no-env.sh"
  cat >"$probe" <<EOF
set -e
HOME=$EMPTY_HOME
VOLUME_ARGS=""
MOUNT_ARGS=""
FILE_MOUNTS=""
DEPLOY_KEY_ARGS=""
STAGING_ROOT=$TEST_TMP/staging-no-env
mkdir -p "\$STAGING_ROOT"
unset WRIX_DEPLOY_KEY
unset WRIX_SIGNING_KEY
source $linux_block
echo "DEPLOY_KEY_ARGS=\$DEPLOY_KEY_ARGS"
EOF

  local out err
  out=$(bash "$probe" 2>"$TEST_TMP/no-env.err") || {
    fail "Linux: env-unset block exited non-zero"
    sed 's/^/    /' "$TEST_TMP/no-env.err" >&2
    return
  }
  if ! grep -qF "DEPLOY_KEY_ARGS=" <<<"$out"; then
    fail "Linux: env-unset block did not set DEPLOY_KEY_ARGS"
    return
  fi
  # No keys staged → DEPLOY_KEY_ARGS should be empty (no -e overrides).
  local args="${out#DEPLOY_KEY_ARGS=}"
  if [[ -n "$args" ]]; then
    fail "Linux: env-unset with empty \$HOME should leave DEPLOY_KEY_ARGS empty, got: $args"
    return
  fi
  pass "Linux: env-unset with empty \$HOME falls through without fail-loud"
}

test_no_env_no_fail

echo
echo "Results: $PASSED passed, $FAILED failed"
[[ "$FAILED" -eq 0 ]]
