#!/usr/bin/env bash
# Verifier for criterion 114 of specs/sandbox.md:
#
#   With NET_ADMIN available, WRIX_NETWORK=limit restricts outbound to the
#   merged allowlist; without NET_ADMIN, limit mode logs a warning and falls
#   back to open network. Any value other than open|limit errors before the
#   container starts.
#
# Two contracts under test, both runnable anywhere (no KVM / no microVM):
#   1. Validation contract — the launcher rejects unknown WRIX_NETWORK
#      values with the documented error before any container work.
#   2. Fallback contract — the Linux entrypoint's filter block warns and
#      proceeds (rather than aborts) when iptables cannot acquire NET_ADMIN.
#
# Enforcement of the limit allowlist inside the container is only meaningful
# when NET_ADMIN is available (microVM on macOS, WRIX_MICROVM=1 on Linux).
# That path is exercised by tests/darwin/network.nix (allowlist resolution).
# This script skips (exit 77) any assertion that would require a real
# NET_ADMIN-capable container.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
LINUX_LAUNCHER_NIX="$REPO_ROOT/lib/sandbox/linux/default.nix"
DARWIN_LAUNCHER_NIX="$REPO_ROOT/lib/sandbox/darwin/default.nix"
LINUX_ENTRYPOINT="$REPO_ROOT/lib/sandbox/linux/entrypoint.sh"

TEST_TMP=$(mktemp -d -t wrix-network-modes.XXXXXX)
trap 'rm -rf "$TEST_TMP"' EXIT

PASSED=0
FAILED=0

pass() { printf '  PASS: %s\n' "$1"; PASSED=$((PASSED + 1)); }
fail() { printf '  FAIL: %s\n' "$1" >&2; FAILED=$((FAILED + 1)); }

# Extract the WRIX_NETWORK validation block from a writeShellApplication
# `text = ''...''` body in lib/sandbox/{linux,darwin}/default.nix and convert
# Nix's `''$` escape back to a literal `$` so the snippet runs under bash.
# The block runs from the marker comment to the first `esac` at any indent.
extract_validation_block() {
  local source="$1" out="$2"
  awk '
    /# Validate WRIX_NETWORK mode/ { capture = 1 }
    capture { print }
    capture && /^[[:space:]]*esac[[:space:]]*$/ { exit }
  ' "$source" \
    | sed "s/''\\\$/\$/g" \
    > "$out"
}

# Extract the Linux entrypoint's network filtering block: from the marker
# comment to the next `fi` at column 0 (the outer-block terminator).
extract_filter_block() {
  local source="$1" out="$2"
  sed -n '/^# Apply network filtering when WRIX_NETWORK=limit/,/^fi$/p' \
    "$source" > "$out"
}

# ----------------------------------------------------------------------------
# Test 1: Linux launcher rejects WRIX_NETWORK=garbage with documented error
# ----------------------------------------------------------------------------
test_linux_validation_rejects_garbage() {
  local block="$TEST_TMP/linux-validate.sh"
  extract_validation_block "$LINUX_LAUNCHER_NIX" "$block"

  local err="$TEST_TMP/linux-garbage.err"
  if WRIX_NETWORK=garbage bash "$block" 2>"$err"; then
    fail "Linux launcher accepted WRIX_NETWORK=garbage"
    return
  fi
  if ! grep -qF "WRIX_NETWORK must be 'open' or 'limit'" "$err"; then
    fail "Linux launcher missing documented error message: $(cat "$err")"
    return
  fi
  pass "Linux launcher rejects WRIX_NETWORK=garbage with documented error"
}

# ----------------------------------------------------------------------------
# Test 2: Darwin launcher rejects WRIX_NETWORK=garbage with documented error
# ----------------------------------------------------------------------------
test_darwin_validation_rejects_garbage() {
  local block="$TEST_TMP/darwin-validate.sh"
  extract_validation_block "$DARWIN_LAUNCHER_NIX" "$block"

  local err="$TEST_TMP/darwin-garbage.err"
  if WRIX_NETWORK=garbage bash "$block" 2>"$err"; then
    fail "Darwin launcher accepted WRIX_NETWORK=garbage"
    return
  fi
  if ! grep -qF "WRIX_NETWORK must be 'open' or 'limit'" "$err"; then
    fail "Darwin launcher missing documented error message: $(cat "$err")"
    return
  fi
  pass "Darwin launcher rejects WRIX_NETWORK=garbage with documented error"
}

# ----------------------------------------------------------------------------
# Test 3: Both valid modes (open, limit) pass the launcher's validation gate
# ----------------------------------------------------------------------------
test_launcher_accepts_valid_modes() {
  local block="$TEST_TMP/linux-validate.sh"
  extract_validation_block "$LINUX_LAUNCHER_NIX" "$block"

  local mode
  for mode in open limit; do
    if ! WRIX_NETWORK="$mode" bash "$block" 2>"$TEST_TMP/valid-$mode.err"; then
      fail "Linux launcher rejected valid WRIX_NETWORK=$mode: $(cat "$TEST_TMP/valid-$mode.err")"
      return
    fi
  done
  pass "Linux launcher accepts WRIX_NETWORK in {open,limit}"
}

# Absolute path to the bash we're running under. Tests that override PATH
# would otherwise lose the ability to spawn `bash` itself.
BASH_BIN="${BASH:-$(command -v bash)}"

# ----------------------------------------------------------------------------
# Test 4: limit mode without NET_ADMIN warns and proceeds (no exit non-zero)
#
# Simulates the rootless-Linux container case: iptables is present but
# `iptables -P OUTPUT DROP` fails because NET_ADMIN is not granted. The
# entrypoint must log the NET_ADMIN warning and continue, NOT abort the
# session.
# ----------------------------------------------------------------------------
test_limit_falls_back_when_iptables_lacks_net_admin() {
  local block="$TEST_TMP/entrypoint-filter.sh"
  extract_filter_block "$LINUX_ENTRYPOINT" "$block"

  local stub_dir="$TEST_TMP/stub-failing-iptables"
  mkdir -p "$stub_dir"
  cat >"$stub_dir/iptables" <<STUB
#!$BASH_BIN
echo "iptables: Operation not permitted (NET_ADMIN required)" >&2
exit 1
STUB
  chmod +x "$stub_dir/iptables"
  cp "$stub_dir/iptables" "$stub_dir/ip6tables"

  local err="$TEST_TMP/fallback-no-netadmin.err"
  if ! PATH="$stub_dir" \
       WRIX_NETWORK=limit \
       WRIX_NETWORK_ALLOWLIST="api.anthropic.com,github.com" \
       "$BASH_BIN" "$block" 2>"$err"; then
    fail "Entrypoint exited non-zero when iptables lacks NET_ADMIN; should warn and fall back"
    sed 's/^/    /' "$err" >&2
    return
  fi
  if ! grep -qF "WRIX_NETWORK=limit requires NET_ADMIN capability" "$err"; then
    fail "Missing NET_ADMIN fallback warning in stderr: $(cat "$err")"
    return
  fi
  pass "limit mode without NET_ADMIN warns and falls back (rootless Linux contract)"
}

# ----------------------------------------------------------------------------
# Test 5: limit mode with no iptables on PATH also falls back gracefully
# ----------------------------------------------------------------------------
test_limit_falls_back_when_iptables_missing() {
  local block="$TEST_TMP/entrypoint-filter.sh"
  extract_filter_block "$LINUX_ENTRYPOINT" "$block"

  local empty_dir="$TEST_TMP/empty-path"
  mkdir -p "$empty_dir"

  local err="$TEST_TMP/fallback-no-iptables.err"
  if ! PATH="$empty_dir" \
       WRIX_NETWORK=limit \
       WRIX_NETWORK_ALLOWLIST="api.anthropic.com" \
       "$BASH_BIN" "$block" 2>"$err"; then
    fail "Entrypoint exited non-zero when iptables is missing; should warn and fall back"
    sed 's/^/    /' "$err" >&2
    return
  fi
  if ! grep -qF "iptables not available, network filtering disabled" "$err"; then
    fail "Missing iptables-not-available warning in stderr: $(cat "$err")"
    return
  fi
  pass "limit mode with missing iptables warns and falls back"
}

# ----------------------------------------------------------------------------
# Test 6: NET_ADMIN-required enforcement assertion (skipped without microVM)
#
# The actual iptables-allowlist enforcement contract requires NET_ADMIN —
# a rootless container can't grant it, so we skip rather than emit a false
# negative. tests/darwin/network.nix exercises the allowlist-resolution
# path end-to-end on a NET_ADMIN-capable boundary.
# ----------------------------------------------------------------------------
test_enforcement_requires_net_admin_capability() {
  if [[ "$(uname -s)" = "Darwin" ]]; then
    echo "  SKIP: NET_ADMIN enforcement covered by tests/darwin/network.nix on Darwin"
    return 77
  fi
  if [[ ! -e /dev/kvm ]]; then
    echo "  SKIP: NET_ADMIN enforcement requires /dev/kvm (WRIX_MICROVM=1) or macOS"
    return 77
  fi
  if ! command -v podman >/dev/null 2>&1; then
    echo "  SKIP: NET_ADMIN enforcement requires podman + krun runtime"
    return 77
  fi
  echo "  SKIP: NET_ADMIN enforcement is a microVM/macOS contract not exercised in-process"
  return 77
}

ALL_TESTS=(
  test_linux_validation_rejects_garbage
  test_darwin_validation_rejects_garbage
  test_launcher_accepts_valid_modes
  test_limit_falls_back_when_iptables_lacks_net_admin
  test_limit_falls_back_when_iptables_missing
  test_enforcement_requires_net_admin_capability
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
