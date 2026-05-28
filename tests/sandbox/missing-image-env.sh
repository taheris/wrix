#!/usr/bin/env bash
# Verifier for criterion 118 of specs/sandbox.md:
#
#   `wrapix run` errors at startup with a clear message when
#   WRAPIX_DEFAULT_IMAGE_REF or WRAPIX_DEFAULT_IMAGE_SOURCE is unset.
#
# The check lives in the launcher script bodies in
# lib/sandbox/{linux,darwin}/default.nix. We extract the image-resolution
# block from each source and execute it under bash, asserting:
#   1. Both vars unset → exit 1, stderr names both vars.
#   2. Only WRAPIX_DEFAULT_IMAGE_REF unset → exit 1, stderr names both vars.
#   3. Only WRAPIX_DEFAULT_IMAGE_SOURCE unset → exit 1, stderr names both vars.
#   4. Both set → exit 0 (the validation gate doesn't false-trigger).
#   5. SUBCOMMAND=spawn bypasses the check entirely.
#
# This mirrors tests/sandbox/network-modes.sh: extracting the exact block
# from source executes the contract code without needing to build the
# launcher derivation, which would pull in podman / Apple container CLI.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
LINUX_LAUNCHER_NIX="$REPO_ROOT/lib/sandbox/linux/default.nix"
DARWIN_LAUNCHER_NIX="$REPO_ROOT/lib/sandbox/darwin/default.nix"

TEST_TMP=$(mktemp -d -t wrapix-missing-image-env.XXXXXX)
trap 'rm -rf "$TEST_TMP"' EXIT

PASSED=0
FAILED=0

pass() { printf '  PASS: %s\n' "$1"; PASSED=$((PASSED + 1)); }
fail() { printf '  FAIL: %s\n' "$1" >&2; FAILED=$((FAILED + 1)); }

# Absolute path to the bash we're running under. Tests that override env
# would otherwise lose the ability to spawn `bash` itself.
BASH_BIN="${BASH:-$(command -v bash)}"

# Extract the image-resolution block from a launcher source file in
# lib/sandbox/{linux,darwin}/default.nix and convert Nix's `''$` escape
# back to a literal `$` so the snippet runs under bash. The block runs
# from the marker comment through the 2nd standalone `fi` (the inner
# [-z] check + the outer SUBCOMMAND if/else).
extract_image_block() {
  local source="$1" out="$2"
  awk '
    /# Image is supplied to the launcher at runtime/ { capture = 1 }
    capture { print }
    capture && /^[[:space:]]*fi[[:space:]]*$/ {
      fi_count++
      if (fi_count == 2) exit
    }
  ' "$source" \
    | sed "s/''\\\$/\$/g" \
    > "$out"
}

# Build a self-contained runnable script that sets the launcher-shell
# preamble (the writeShellApplication / writeShellScriptBin scripts both
# run under `set -euo pipefail`), defines the SUBCOMMAND and any spawn-
# mode overrides the block reads, then executes the extracted block.
mk_runner() {
  local block="$1" subcommand="$2" out="$3"
  {
    printf '#!%s\n' "$BASH_BIN"
    printf 'set -euo pipefail\n'
    printf 'SUBCOMMAND=%s\n' "$subcommand"
    printf 'IMAGE_OVERRIDE_REF=""\n'
    printf 'IMAGE_OVERRIDE_SOURCE=""\n'
    cat "$block"
  } > "$out"
  chmod +x "$out"
}

# Assert the runner exits non-zero and stderr names both env vars in
# the documented error message.
assert_missing_env_errors() {
  local label="$1" runner="$2"
  shift 2
  local err="$TEST_TMP/${label}.err"
  if env "$@" "$BASH_BIN" "$runner" 2>"$err"; then
    fail "$label: launcher accepted missing image env vars"
    sed 's/^/    /' "$err" >&2
    return 1
  fi
  if ! grep -qF "WRAPIX_DEFAULT_IMAGE_REF" "$err"; then
    fail "$label: stderr missing WRAPIX_DEFAULT_IMAGE_REF: $(cat "$err")"
    return 1
  fi
  if ! grep -qF "WRAPIX_DEFAULT_IMAGE_SOURCE" "$err"; then
    fail "$label: stderr missing WRAPIX_DEFAULT_IMAGE_SOURCE: $(cat "$err")"
    return 1
  fi
  return 0
}

# ----------------------------------------------------------------------------
# Test 1: Linux launcher errors when BOTH image env vars are unset
# ----------------------------------------------------------------------------
test_linux_run_both_unset_errors() {
  local block="$TEST_TMP/linux-image.sh"
  local runner="$TEST_TMP/linux-run-both-unset.sh"
  extract_image_block "$LINUX_LAUNCHER_NIX" "$block"
  mk_runner "$block" "run" "$runner"

  if assert_missing_env_errors "linux-both-unset" "$runner" \
      -u WRAPIX_DEFAULT_IMAGE_REF -u WRAPIX_DEFAULT_IMAGE_SOURCE; then
    pass "Linux: wrapix run with both image env vars unset errors with documented message"
  fi
}

# ----------------------------------------------------------------------------
# Test 2: Linux launcher errors when only WRAPIX_DEFAULT_IMAGE_REF is unset
# ----------------------------------------------------------------------------
test_linux_run_ref_unset_errors() {
  local block="$TEST_TMP/linux-image.sh"
  local runner="$TEST_TMP/linux-run-ref-unset.sh"
  extract_image_block "$LINUX_LAUNCHER_NIX" "$block"
  mk_runner "$block" "run" "$runner"

  if assert_missing_env_errors "linux-ref-unset" "$runner" \
      -u WRAPIX_DEFAULT_IMAGE_REF \
      WRAPIX_DEFAULT_IMAGE_SOURCE=/nix/store/fake.tar.gz; then
    pass "Linux: wrapix run with WRAPIX_DEFAULT_IMAGE_REF unset errors"
  fi
}

# ----------------------------------------------------------------------------
# Test 3: Linux launcher errors when only WRAPIX_DEFAULT_IMAGE_SOURCE is unset
# ----------------------------------------------------------------------------
test_linux_run_source_unset_errors() {
  local block="$TEST_TMP/linux-image.sh"
  local runner="$TEST_TMP/linux-run-source-unset.sh"
  extract_image_block "$LINUX_LAUNCHER_NIX" "$block"
  mk_runner "$block" "run" "$runner"

  if assert_missing_env_errors "linux-source-unset" "$runner" \
      -u WRAPIX_DEFAULT_IMAGE_SOURCE \
      WRAPIX_DEFAULT_IMAGE_REF=wrapix-base:latest; then
    pass "Linux: wrapix run with WRAPIX_DEFAULT_IMAGE_SOURCE unset errors"
  fi
}

# ----------------------------------------------------------------------------
# Test 4: Linux launcher does NOT false-trigger when both vars are set
# ----------------------------------------------------------------------------
test_linux_run_both_set_passes() {
  local block="$TEST_TMP/linux-image.sh"
  local runner="$TEST_TMP/linux-run-both-set.sh"
  extract_image_block "$LINUX_LAUNCHER_NIX" "$block"
  mk_runner "$block" "run" "$runner"

  local err="$TEST_TMP/linux-both-set.err"
  if ! env \
        WRAPIX_DEFAULT_IMAGE_REF=wrapix-base:latest \
        WRAPIX_DEFAULT_IMAGE_SOURCE=/nix/store/fake.tar.gz \
        "$BASH_BIN" "$runner" 2>"$err"; then
    fail "Linux: launcher rejected wrapix run with both image env vars set: $(cat "$err")"
    return
  fi
  pass "Linux: wrapix run with both image env vars set passes the validation gate"
}

# ----------------------------------------------------------------------------
# Test 5: Linux SUBCOMMAND=spawn bypasses the env check (uses SpawnConfig)
# ----------------------------------------------------------------------------
test_linux_spawn_bypasses_env_check() {
  local block="$TEST_TMP/linux-image.sh"
  local runner="$TEST_TMP/linux-spawn-bypass.sh"
  extract_image_block "$LINUX_LAUNCHER_NIX" "$block"
  mk_runner "$block" "spawn" "$runner"

  local err="$TEST_TMP/linux-spawn-bypass.err"
  if ! env -u WRAPIX_DEFAULT_IMAGE_REF -u WRAPIX_DEFAULT_IMAGE_SOURCE \
        "$BASH_BIN" "$runner" 2>"$err"; then
    fail "Linux: spawn mode incorrectly triggered the run-mode env check: $(cat "$err")"
    return
  fi
  pass "Linux: wrapix spawn bypasses the WRAPIX_DEFAULT_IMAGE_* env check"
}

# ----------------------------------------------------------------------------
# Test 6: Darwin launcher errors when BOTH image env vars are unset
# ----------------------------------------------------------------------------
test_darwin_run_both_unset_errors() {
  local block="$TEST_TMP/darwin-image.sh"
  local runner="$TEST_TMP/darwin-run-both-unset.sh"
  extract_image_block "$DARWIN_LAUNCHER_NIX" "$block"
  mk_runner "$block" "run" "$runner"

  if assert_missing_env_errors "darwin-both-unset" "$runner" \
      -u WRAPIX_DEFAULT_IMAGE_REF -u WRAPIX_DEFAULT_IMAGE_SOURCE; then
    pass "Darwin: wrapix run with both image env vars unset errors with documented message"
  fi
}

# ----------------------------------------------------------------------------
# Test 7: Darwin launcher errors when only WRAPIX_DEFAULT_IMAGE_REF is unset
# ----------------------------------------------------------------------------
test_darwin_run_ref_unset_errors() {
  local block="$TEST_TMP/darwin-image.sh"
  local runner="$TEST_TMP/darwin-run-ref-unset.sh"
  extract_image_block "$DARWIN_LAUNCHER_NIX" "$block"
  mk_runner "$block" "run" "$runner"

  if assert_missing_env_errors "darwin-ref-unset" "$runner" \
      -u WRAPIX_DEFAULT_IMAGE_REF \
      WRAPIX_DEFAULT_IMAGE_SOURCE=/nix/store/fake.tar.gz; then
    pass "Darwin: wrapix run with WRAPIX_DEFAULT_IMAGE_REF unset errors"
  fi
}

# ----------------------------------------------------------------------------
# Test 8: Darwin launcher errors when only WRAPIX_DEFAULT_IMAGE_SOURCE is unset
# ----------------------------------------------------------------------------
test_darwin_run_source_unset_errors() {
  local block="$TEST_TMP/darwin-image.sh"
  local runner="$TEST_TMP/darwin-run-source-unset.sh"
  extract_image_block "$DARWIN_LAUNCHER_NIX" "$block"
  mk_runner "$block" "run" "$runner"

  if assert_missing_env_errors "darwin-source-unset" "$runner" \
      -u WRAPIX_DEFAULT_IMAGE_SOURCE \
      WRAPIX_DEFAULT_IMAGE_REF=wrapix-base:latest; then
    pass "Darwin: wrapix run with WRAPIX_DEFAULT_IMAGE_SOURCE unset errors"
  fi
}

# ----------------------------------------------------------------------------
# Test 9: Darwin launcher does NOT false-trigger when both vars are set
# ----------------------------------------------------------------------------
test_darwin_run_both_set_passes() {
  local block="$TEST_TMP/darwin-image.sh"
  local runner="$TEST_TMP/darwin-run-both-set.sh"
  extract_image_block "$DARWIN_LAUNCHER_NIX" "$block"
  mk_runner "$block" "run" "$runner"

  local err="$TEST_TMP/darwin-both-set.err"
  if ! env \
        WRAPIX_DEFAULT_IMAGE_REF=wrapix-base:latest \
        WRAPIX_DEFAULT_IMAGE_SOURCE=/nix/store/fake.tar.gz \
        "$BASH_BIN" "$runner" 2>"$err"; then
    fail "Darwin: launcher rejected wrapix run with both image env vars set: $(cat "$err")"
    return
  fi
  pass "Darwin: wrapix run with both image env vars set passes the validation gate"
}

# ----------------------------------------------------------------------------
# Test 10: Darwin SUBCOMMAND=spawn bypasses the env check
# ----------------------------------------------------------------------------
test_darwin_spawn_bypasses_env_check() {
  local block="$TEST_TMP/darwin-image.sh"
  local runner="$TEST_TMP/darwin-spawn-bypass.sh"
  extract_image_block "$DARWIN_LAUNCHER_NIX" "$block"
  mk_runner "$block" "spawn" "$runner"

  local err="$TEST_TMP/darwin-spawn-bypass.err"
  if ! env -u WRAPIX_DEFAULT_IMAGE_REF -u WRAPIX_DEFAULT_IMAGE_SOURCE \
        "$BASH_BIN" "$runner" 2>"$err"; then
    fail "Darwin: spawn mode incorrectly triggered the run-mode env check: $(cat "$err")"
    return
  fi
  pass "Darwin: wrapix spawn bypasses the WRAPIX_DEFAULT_IMAGE_* env check"
}

ALL_TESTS=(
  test_linux_run_both_unset_errors
  test_linux_run_ref_unset_errors
  test_linux_run_source_unset_errors
  test_linux_run_both_set_passes
  test_linux_spawn_bypasses_env_check
  test_darwin_run_both_unset_errors
  test_darwin_run_ref_unset_errors
  test_darwin_run_source_unset_errors
  test_darwin_run_both_set_passes
  test_darwin_spawn_bypasses_env_check
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
