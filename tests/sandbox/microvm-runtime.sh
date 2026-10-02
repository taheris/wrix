#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$REPO_ROOT/tests/lib/live-sandbox.sh"

TEST_TMP="$(mktemp -d -t wrix-microvm-runtime.XXXXXX)"

cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

run_microvm_probe() {
  local command_line="$1"
  local output status=0

  output=$(WRIX_MICROVM=1 wrix_run_with_pty "$command_line" 2>&1) || status=$?
  if [[ "$status" -ne 0 ]]; then
    printf 'FAIL: microVM launcher exited with status %s:\n%s\n' "$status" "$output" >&2
    return "$status"
  fi
  printf '%s\n' "$output"
}

test_microvm_probe_reports_launch_failure() {
  local command_line output status=0
  local -a command=(bash -c 'printf "microvm-launch-failed\n" >&2; exit 42')

  [[ "$(uname -s)" == "Linux" ]] || wrix_live_skip "Linux PTY exit-status verifier"
  printf -v command_line '%q ' "${command[@]}"
  output=$(run_microvm_probe "$command_line" 2>&1) || status=$?
  [[ "$status" -eq 42 ]] || fail "PTY probe lost the launcher exit status: $status"
  [[ "$output" == *'microVM launcher exited with status 42:'*'microvm-launch-failed'* ]] \
    || fail "PTY probe discarded the launcher diagnostics: $output"
}

test_linux_microvm_runtime() {
  local command_line output sandbox workspace
  local -a command
  wrix_require_live_sandbox_linux
  [[ -e /dev/kvm ]] || wrix_live_skip "KVM device is required for the live microVM verifier"
  cd "$REPO_ROOT"

  sandbox=$(wrix_build_packaged_live_sandbox)
  workspace="$TEST_TMP/workspace"
  mkdir -p "$workspace"

  cat >"$workspace/assert-microvm.sh" <<'INNER'
#!/usr/bin/env bash
set -euo pipefail

[[ -x /krun-relay ]]
[[ -x /krun-init.sh ]]
[[ -f /lib/libfakeuid.so ]]
[[ "${LD_PRELOAD:-}" == "/lib/libfakeuid.so" ]]
[[ "${WRIX_TERM_ROWS:-}" =~ ^[0-9]+$ ]]
[[ "${WRIX_TERM_COLS:-}" =~ ^[0-9]+$ ]]
[[ "${1:-}" == "alpha" ]]
[[ "${2:-}" == "two words" ]]
printf 'MICROVM_BOUNDARY_OK=%s|%s\n' "$1" "$2"
INNER
  chmod +x "$workspace/assert-microvm.sh"

  command=(
    "$sandbox/bin/wrix" run "$workspace"
    /workspace/assert-microvm.sh alpha "two words"
  )
  printf -v command_line '%q ' "${command[@]}"
  output=$(run_microvm_probe "$command_line") || return

  if [[ "$output" != *"MICROVM_BOUNDARY_OK=alpha|two words"* ]]; then
    fail "live krun microVM did not reach the relay/init/libfakeuid boundary: $output"
    return 1
  fi
  printf 'PASS: live launcher reached the krun relay/init/libfakeuid boundary\n'
}

case "${1:-}" in
  ""|test_linux_microvm_runtime)
    test_microvm_probe_reports_launch_failure
    test_linux_microvm_runtime
    ;;
  test_microvm_probe_reports_launch_failure) test_microvm_probe_reports_launch_failure ;;
  *) fail "unknown microVM verifier: $1"; exit 64 ;;
esac
