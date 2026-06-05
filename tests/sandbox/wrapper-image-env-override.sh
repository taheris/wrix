#!/usr/bin/env bash
# Verifier for the mkSandbox wrapper's image-ref env-var hand-off
# contract:
#
#   The wrapped `wrix` binary at $out/bin/wrix must let an
#   orchestrator pre-export WRIX_DEFAULT_IMAGE_REF and
#   WRIX_DEFAULT_IMAGE_SOURCE before exec'ing it — the caller's
#   values must reach the launcher, NOT the wrapper's baked defaults.
#   WRIX_AGENT, by contrast, MUST stay clobbered: the wrapper is
#   bound to its built `(profile × agent)` image variant and an
#   exec-time override would break that binding.
#
# Two pieces, jointly load-bearing:
#
#   1. Production-source assertion — `lib/sandbox/default.nix` uses
#      `makeWrapper --set-default` for the two image env vars and
#      `--set` for WRIX_AGENT. The [check?] grep target on its own
#      cannot distinguish `--set WRIX_AGENT` from
#      `--set-default WRIX_AGENT` (substring), so the directive
#      assertion is paired with the behavioral assertion below.
#
#   2. Behavioral assertion — the bash idioms makeWrapper emits for
#      `--set` and `--set-default` are reproduced verbatim in a
#      hermetic tmpdir wrapper over a printenv stub, exercised under
#      caller-set / caller-unset env. This proves the semantics the
#      production directives rely on:
#
#        --set VAR VAL          → export VAR='VAL'
#                                  (caller value clobbered)
#        --set-default VAR VAL  → if [[ -z "${VAR-}" ]]; then
#                                    export VAR='VAL'
#                                 fi
#                                  (caller value wins)
#
# This deliberately avoids `nix build`: building the live wrapped
# package would pull every flake input (including private ones), which
# is fragile in restricted environments. The two assertions together
# bind the production source to the documented makeWrapper output and
# verify the output's semantics — the same chain a live-wrapper build
# would prove.
#
# Companion verifier: tests/sandbox/missing-image-env.sh (criterion
# 130) exercises the bare-launcher path (both vars unset → fail-loud)
# and must not regress when the wrapper switches to --set-default.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
WRAPPER_NIX="$REPO_ROOT/lib/sandbox/default.nix"
LINUX_LAUNCHER_NIX="$REPO_ROOT/lib/sandbox/linux/default.nix"
DARWIN_LAUNCHER_NIX="$REPO_ROOT/lib/sandbox/darwin/default.nix"

TEST_TMP=$(mktemp -d -t wrix-wrapper-image-env-override.XXXXXX)
trap 'rm -rf "$TEST_TMP"' EXIT

BASH_BIN="${BASH:-$(command -v bash)}"

PASSED=0
FAILED=0

pass() { printf '  PASS: %s\n' "$1"; PASSED=$((PASSED + 1)); }
fail() { printf '  FAIL: %s\n' "$1" >&2; FAILED=$((FAILED + 1)); }

BAKED_AGENT='baked-agent'
BAKED_REF='baked-ref'
BAKED_SOURCE='/nix/store/fake-baked-source'

# Stub launcher — what `makeWrapper` wraps in production. Prints the
# three env vars the wrapper bakes, one per line, with `<UNSET>` when
# the var did not survive the wrapper's env-setup section.
STUB="$TEST_TMP/stub-launcher"
cat > "$STUB" <<EOF
#!$BASH_BIN
set -u
printf 'WRIX_AGENT=%s\n' "\${WRIX_AGENT-<UNSET>}"
printf 'WRIX_DEFAULT_IMAGE_REF=%s\n' "\${WRIX_DEFAULT_IMAGE_REF-<UNSET>}"
printf 'WRIX_DEFAULT_IMAGE_SOURCE=%s\n' "\${WRIX_DEFAULT_IMAGE_SOURCE-<UNSET>}"
EOF
chmod +x "$STUB"

# Hermetic wrapper — reproduces the env-setup section that
# `makeWrapper --set WRIX_AGENT ... --set-default WRIX_DEFAULT_IMAGE_REF ...
# --set-default WRIX_DEFAULT_IMAGE_SOURCE ...` writes. The idioms
# are the documented output of nixpkgs
# pkgs/build-support/setup-hooks/make-wrapper.sh.
WRAPPER="$TEST_TMP/wrapper"
cat > "$WRAPPER" <<EOF
#!$BASH_BIN
export WRIX_AGENT='$BAKED_AGENT'
if [[ -z "\${WRIX_DEFAULT_IMAGE_REF-}" ]]; then
    export WRIX_DEFAULT_IMAGE_REF='$BAKED_REF'
fi
if [[ -z "\${WRIX_DEFAULT_IMAGE_SOURCE-}" ]]; then
    export WRIX_DEFAULT_IMAGE_SOURCE='$BAKED_SOURCE'
fi
exec "$STUB" "\$@"
EOF
chmod +x "$WRAPPER"

# Run the wrapper under the given env modifications and capture stdout.
run_wrapper() {
  local out="$1"
  shift
  env "$@" "$WRAPPER" > "$out"
}

# Caller-unset → wrapper-baked defaults reach the stub for both image vars.
test_caller_unset_uses_baked_defaults() {
  local out="$TEST_TMP/unset.out"
  run_wrapper "$out" \
    -u WRIX_DEFAULT_IMAGE_REF \
    -u WRIX_DEFAULT_IMAGE_SOURCE \
    -u WRIX_AGENT
  if ! grep -qxF "WRIX_DEFAULT_IMAGE_REF=$BAKED_REF" "$out"; then
    fail "unset REF: launcher saw $(grep '^WRIX_DEFAULT_IMAGE_REF=' "$out"); expected $BAKED_REF"
    return 1
  fi
  if ! grep -qxF "WRIX_DEFAULT_IMAGE_SOURCE=$BAKED_SOURCE" "$out"; then
    fail "unset SOURCE: launcher saw $(grep '^WRIX_DEFAULT_IMAGE_SOURCE=' "$out"); expected $BAKED_SOURCE"
    return 1
  fi
  pass "Caller-unset → both image vars resolve to wrapper-baked defaults"
}

# Caller-set REF → REF=caller's; SOURCE remains the baked default.
test_caller_set_ref_wins_independently() {
  local out="$TEST_TMP/set-ref.out"
  run_wrapper "$out" \
    -u WRIX_DEFAULT_IMAGE_SOURCE \
    WRIX_DEFAULT_IMAGE_REF=caller-ref
  if ! grep -qxF 'WRIX_DEFAULT_IMAGE_REF=caller-ref' "$out"; then
    fail "caller-set REF: launcher saw $(grep '^WRIX_DEFAULT_IMAGE_REF=' "$out"); expected caller-ref"
    return 1
  fi
  if ! grep -qxF "WRIX_DEFAULT_IMAGE_SOURCE=$BAKED_SOURCE" "$out"; then
    fail "caller-set REF: SOURCE unexpectedly clobbered; saw $(grep '^WRIX_DEFAULT_IMAGE_SOURCE=' "$out")"
    return 1
  fi
  pass "Caller-set REF + unset SOURCE → REF=caller's, SOURCE=baked default"
}

# Caller-set SOURCE → SOURCE=caller's; REF remains the baked default.
test_caller_set_source_wins_independently() {
  local out="$TEST_TMP/set-source.out"
  run_wrapper "$out" \
    -u WRIX_DEFAULT_IMAGE_REF \
    WRIX_DEFAULT_IMAGE_SOURCE=/nix/store/caller-source
  if ! grep -qxF "WRIX_DEFAULT_IMAGE_REF=$BAKED_REF" "$out"; then
    fail "caller-set SOURCE: REF unexpectedly clobbered; saw $(grep '^WRIX_DEFAULT_IMAGE_REF=' "$out")"
    return 1
  fi
  if ! grep -qxF 'WRIX_DEFAULT_IMAGE_SOURCE=/nix/store/caller-source' "$out"; then
    fail "caller-set SOURCE: launcher saw $(grep '^WRIX_DEFAULT_IMAGE_SOURCE=' "$out"); expected /nix/store/caller-source"
    return 1
  fi
  pass "Caller-set SOURCE + unset REF → SOURCE=caller's, REF=baked default"
}

# Caller-set both image vars → both flow through; nothing clobbered.
test_caller_set_both_image_vars_win() {
  local out="$TEST_TMP/set-both.out"
  run_wrapper "$out" \
    WRIX_DEFAULT_IMAGE_REF=caller-ref \
    WRIX_DEFAULT_IMAGE_SOURCE=/nix/store/caller-source
  if ! grep -qxF 'WRIX_DEFAULT_IMAGE_REF=caller-ref' "$out"; then
    fail "caller-set both: REF saw $(grep '^WRIX_DEFAULT_IMAGE_REF=' "$out"); expected caller-ref"
    return 1
  fi
  if ! grep -qxF 'WRIX_DEFAULT_IMAGE_SOURCE=/nix/store/caller-source' "$out"; then
    fail "caller-set both: SOURCE saw $(grep '^WRIX_DEFAULT_IMAGE_SOURCE=' "$out"); expected /nix/store/caller-source"
    return 1
  fi
  pass "Caller-set both image vars → both reach the launcher verbatim"
}

# WRIX_AGENT is NOT caller-overridable. Per the bead Note 4, the
# directive grep cannot prove non-overridability (`--set` substring-
# matches `--set-default`); this is the load-bearing behavioral check.
test_agent_is_not_caller_overridable() {
  local out="$TEST_TMP/agent-set.out"
  run_wrapper "$out" WRIX_AGENT=caller-agent
  if ! grep -qxF "WRIX_AGENT=$BAKED_AGENT" "$out"; then
    fail "WRIX_AGENT was caller-overridable: launcher saw $(grep '^WRIX_AGENT=' "$out"); expected $BAKED_AGENT"
    return 1
  fi
  pass "Caller-set WRIX_AGENT is clobbered → launcher sees baked agent value"
}

# Production source uses the directives whose emitted idioms the
# hermetic wrapper above reproduces. Without this assertion, the
# behavioral test would still pass even if the production code
# regressed to `--set` for the image vars (or to `--set-default` for
# WRIX_AGENT). The directive assertion + behavioral assertion are
# jointly load-bearing.
test_production_source_uses_correct_directives() {
  if ! grep -nE -- '--set-default[[:space:]]+WRIX_DEFAULT_IMAGE_REF' "$WRAPPER_NIX" >/dev/null; then
    fail "lib/sandbox/default.nix: WRIX_DEFAULT_IMAGE_REF must use --set-default (caller-overridable)"
    return 1
  fi
  if ! grep -nE -- '--set-default[[:space:]]+WRIX_DEFAULT_IMAGE_SOURCE' "$WRAPPER_NIX" >/dev/null; then
    fail "lib/sandbox/default.nix: WRIX_DEFAULT_IMAGE_SOURCE must use --set-default (caller-overridable)"
    return 1
  fi
  if grep -nE -- '--set-default[[:space:]]+WRIX_AGENT' "$WRAPPER_NIX" >/dev/null; then
    fail "lib/sandbox/default.nix: WRIX_AGENT must NOT use --set-default — the wrapper is bound to its (profile × agent) image"
    return 1
  fi
  if ! grep -nE -- '--set[[:space:]]+WRIX_AGENT' "$WRAPPER_NIX" >/dev/null; then
    fail "lib/sandbox/default.nix: WRIX_AGENT must use --set (unconditional)"
    return 1
  fi
  pass "lib/sandbox/default.nix uses --set-default for image vars, --set for WRIX_AGENT"
}

test_launchers_pass_agent_to_container() {
  if ! grep -nF -- '-e "WRIX_AGENT=$WRIX_AGENT"' "$LINUX_LAUNCHER_NIX" >/dev/null; then
    fail "lib/sandbox/linux/default.nix: launcher must pass WRIX_AGENT into podman env"
    return 1
  fi
  if ! grep -nF -- '-e "WRIX_AGENT=$WRIX_AGENT"' "$DARWIN_LAUNCHER_NIX" >/dev/null; then
    fail "lib/sandbox/darwin/default.nix: launcher must pass WRIX_AGENT into container env"
    return 1
  fi
  pass "Linux and Darwin launchers pass WRIX_AGENT into the container"
}

ALL_TESTS=(
  test_caller_unset_uses_baked_defaults
  test_caller_set_ref_wins_independently
  test_caller_set_source_wins_independently
  test_caller_set_both_image_vars_win
  test_agent_is_not_caller_overridable
  test_production_source_uses_correct_directives
  test_launchers_pass_agent_to_container
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
