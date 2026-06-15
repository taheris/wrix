#!/usr/bin/env bash
# Verifier for specs/security.md § Deploy & Signing Keys / "Spawn mode
# requires both keys":
#
#   Under `wrix spawn`, when a deploy key or signing key does not resolve
#   (no env pointer and no $HOME/.ssh/deploy_keys/ fallback), the launcher
#   exits non-zero with a stderr message naming the unresolved key, before
#   the container is started; interactive `wrix run` still boots without
#   keys under the same condition.
#
# The launcher's key-resolution block is extracted from the
# writeShellApplication `text = ''…''` body in lib/sandbox/{linux,darwin}/
# default.nix (same extraction as key-env-missing-file.sh): the `''$` Nix
# escape is converted back to `$`, and the Nix interpolations
# `${deployKeyExpr}` / `${sshConfig.containerKeyDir}` are substituted with
# concrete values. We drive the block with SUBCOMMAND=spawn / run and assert
# the fail-loud (spawn) vs. permissive (run) behaviour.
#
# "No container started" is implicit: the extracted block is upstream of the
# launcher's `exec podman run` / `container run` call site, so a non-zero
# exit inside the block proves no container starts.
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

TEST_TMP=$(mktemp -d -t wrix-spawn-keys.XXXXXX)
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
  # SC2016: single quotes are intentional — the `${…}` patterns are Nix
  # interpolation tokens we want sed to match literally.
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
REAL_DEPLOY_KEY="$TEST_TMP/real-deploy-key"
echo "fake-key-material" > "$REAL_DEPLOY_KEY"

# Common preamble for a sourced block probe: empty mount/staging state, a
# non-Pi agent, and an empty $HOME so no fallback key resolves unless we
# provide an env pointer.
write_probe_preamble() {
  local probe="$1" label="$2"
  cat >"$probe" <<EOF
set -euo pipefail
HOME="$EMPTY_HOME"
WRIX_AGENT=direct
unset WRIX_PI_AUTH_FILE
VOLUME_ARGS=""
MOUNT_ARGS=""
FILE_MOUNTS=""
DEPLOY_KEY_ARGS=""
STAGING_ROOT="$TEST_TMP/staging-$label"
mkdir -p "\$STAGING_ROOT"
EOF
}

# spawn + nothing resolves → fail loud naming the deploy key (checked first).
assert_spawn_fails_no_keys() {
  local label="$1" block="$2"
  local probe="$TEST_TMP/$label.sh"
  write_probe_preamble "$probe" "$label"
  cat >>"$probe" <<EOF
SUBCOMMAND=spawn
unset WRIX_DEPLOY_KEY
unset WRIX_SIGNING_KEY
source $block
echo "REACHED_END_UNEXPECTEDLY"
EOF
  local out="$TEST_TMP/$label.out" err="$TEST_TMP/$label.err"
  if bash "$probe" >"$out" 2>"$err"; then
    fail "$label: spawn with no keys exited 0 (expected fail-loud)"
    return
  fi
  if grep -qF "REACHED_END_UNEXPECTEDLY" "$out"; then
    fail "$label: spawn fell through past the key guard"
    return
  fi
  if ! grep -qF "no deploy key resolved" "$err"; then
    fail "$label: stderr does not say 'no deploy key resolved'. stderr: $(cat "$err")"
    return
  fi
  pass "$label: spawn with no keys fails loud naming the deploy key"
}

# spawn + deploy resolves but signing missing → fail loud naming signing key.
assert_spawn_fails_no_signing() {
  local label="$1" block="$2"
  local probe="$TEST_TMP/$label.sh"
  write_probe_preamble "$probe" "$label"
  cat >>"$probe" <<EOF
SUBCOMMAND=spawn
WRIX_DEPLOY_KEY=$REAL_DEPLOY_KEY
unset WRIX_SIGNING_KEY
source $block
echo "REACHED_END_UNEXPECTEDLY"
EOF
  local out="$TEST_TMP/$label.out" err="$TEST_TMP/$label.err"
  if bash "$probe" >"$out" 2>"$err"; then
    fail "$label: spawn with deploy-only exited 0 (expected fail-loud on signing)"
    return
  fi
  if grep -qF "REACHED_END_UNEXPECTEDLY" "$out"; then
    fail "$label: spawn fell through past the signing-key guard"
    return
  fi
  if ! grep -qF "no signing key resolved" "$err"; then
    fail "$label: stderr does not say 'no signing key resolved'. stderr: $(cat "$err")"
    return
  fi
  pass "$label: spawn with deploy-only fails loud naming the signing key"
}

# spawn + WRIX_GIT_SIGN=0 + deploy resolves → signing not required, end reached.
assert_spawn_nosign_deploy_only() {
  local label="$1" block="$2"
  local probe="$TEST_TMP/$label.sh"
  write_probe_preamble "$probe" "$label"
  cat >>"$probe" <<EOF
SUBCOMMAND=spawn
WRIX_GIT_SIGN=0
WRIX_DEPLOY_KEY=$REAL_DEPLOY_KEY
unset WRIX_SIGNING_KEY
source $block
echo "REACHED_END_OK"
EOF
  local out="$TEST_TMP/$label.out" err="$TEST_TMP/$label.err"
  if ! bash "$probe" >"$out" 2>"$err"; then
    fail "$label: spawn WRIX_GIT_SIGN=0 deploy-only exited non-zero (expected pass). stderr: $(cat "$err")"
    return
  fi
  if ! grep -qF "REACHED_END_OK" "$out"; then
    fail "$label: spawn WRIX_GIT_SIGN=0 deploy-only did not reach end of block"
    return
  fi
  pass "$label: spawn WRIX_GIT_SIGN=0 with deploy key boots (signing not required)"
}

# spawn + WRIX_GIT_SIGN=0 + no deploy → deploy still required, fail loud.
assert_spawn_nosign_still_needs_deploy() {
  local label="$1" block="$2"
  local probe="$TEST_TMP/$label.sh"
  write_probe_preamble "$probe" "$label"
  cat >>"$probe" <<EOF
SUBCOMMAND=spawn
WRIX_GIT_SIGN=0
unset WRIX_DEPLOY_KEY
unset WRIX_SIGNING_KEY
source $block
echo "REACHED_END_UNEXPECTEDLY"
EOF
  local out="$TEST_TMP/$label.out" err="$TEST_TMP/$label.err"
  if bash "$probe" >"$out" 2>"$err"; then
    fail "$label: spawn WRIX_GIT_SIGN=0 no deploy exited 0 (expected fail-loud)"
    return
  fi
  if grep -qF "REACHED_END_UNEXPECTEDLY" "$out"; then
    fail "$label: spawn WRIX_GIT_SIGN=0 fell through past the deploy guard"
    return
  fi
  if ! grep -qF "no deploy key resolved" "$err"; then
    fail "$label: stderr does not say 'no deploy key resolved'. stderr: $(cat "$err")"
    return
  fi
  pass "$label: spawn WRIX_GIT_SIGN=0 still fails loud on missing deploy key"
}

# run + nothing resolves → permissive (rule 3): block exits 0, no fail-loud.
assert_run_permits_no_keys() {
  local label="$1" block="$2"
  local probe="$TEST_TMP/$label.sh"
  write_probe_preamble "$probe" "$label"
  cat >>"$probe" <<EOF
SUBCOMMAND=run
unset WRIX_DEPLOY_KEY
unset WRIX_SIGNING_KEY
source $block
echo "REACHED_END_OK"
EOF
  local out="$TEST_TMP/$label.out" err="$TEST_TMP/$label.err"
  if ! bash "$probe" >"$out" 2>"$err"; then
    fail "$label: run with no keys exited non-zero (expected permissive). stderr: $(cat "$err")"
    return
  fi
  if ! grep -qF "REACHED_END_OK" "$out"; then
    fail "$label: run did not reach end of block"
    return
  fi
  pass "$label: run with no keys boots without fail-loud (rule 3 preserved)"
}

linux_block="$TEST_TMP/linux.sh"
darwin_block="$TEST_TMP/darwin.sh"
extract_keyresolve_block "$LINUX_LAUNCHER_NIX" "$linux_block"
extract_keyresolve_block "$DARWIN_LAUNCHER_NIX" "$darwin_block"

assert_spawn_fails_no_keys             linux-spawn-no-keys       "$linux_block"
assert_spawn_fails_no_signing          linux-spawn-no-signing    "$linux_block"
assert_spawn_nosign_deploy_only        linux-spawn-nosign-deploy "$linux_block"
assert_spawn_nosign_still_needs_deploy linux-spawn-nosign-nodep  "$linux_block"
assert_run_permits_no_keys             linux-run-no-keys         "$linux_block"
assert_spawn_fails_no_keys             darwin-spawn-no-keys       "$darwin_block"
assert_spawn_fails_no_signing          darwin-spawn-no-signing    "$darwin_block"
assert_spawn_nosign_deploy_only        darwin-spawn-nosign-deploy "$darwin_block"
assert_spawn_nosign_still_needs_deploy darwin-spawn-nosign-nodep  "$darwin_block"
assert_run_permits_no_keys             darwin-run-no-keys         "$darwin_block"

echo
echo "Results: $PASSED passed, $FAILED failed"
[[ "$FAILED" -eq 0 ]]
