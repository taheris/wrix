#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$REPO_ROOT/tests/lib/live-sandbox.sh"

TEST_TMP="$(mktemp -d -t wrix-microvm-runtime.XXXXXX)"
IMAGE_REF=""

cleanup() {
  if [[ -n "$IMAGE_REF" ]]; then
    wrix_remove_image_ref "$IMAGE_REF" >/dev/null 2>&1 || true # best-effort: cleanup must not mask the verifier result.
  fi
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
  local command_line output sandbox workspace profile_config spawn_config status
  local -a command
  wrix_require_live_sandbox_linux
  [[ -c /dev/kvm && -r /dev/kvm && -w /dev/kvm ]] || wrix_live_skip "accessible KVM is required for the live microVM verifier"
  command -v krun >/dev/null || wrix_live_skip "krun is not on PATH"
  cd "$REPO_ROOT"

  sandbox=$(wrix_build_live_launcher)
  profile_config=$(wrix_build_command_profile_config)
  workspace="$TEST_TMP/workspace"
  mkdir -p "$workspace"

  cat >"$workspace/assert-microvm.sh" <<'INNER'
#!/usr/bin/env bash
set -euo pipefail

[[ -x /krun-relay ]]
[[ -x /krun-init.sh ]]
[[ ! -e /lib/libfakeuid.so ]]
[[ -z "${LD_PRELOAD:-}" ]]
[[ "$(id -u)" == 0 ]]
[[ "${IS_SANDBOX:-}" == 1 ]]
[[ "$(stat -c %u:%a /nix/var/nix)" == 0:755 ]]
nix --offline eval --expr '1 + 1' | grep -qx 2
[[ "${WRIX_TERM_ROWS:-}" =~ ^[0-9]+$ ]]
[[ "${WRIX_TERM_COLS:-}" =~ ^[0-9]+$ ]]
[[ "${1:-}" == "alpha" ]]
[[ "${2:-}" == "two words" ]]
if [[ "${WRIX_STDIO:-}" == 1 ]]; then
  IFS= read -r line
  [[ "$line" == 'microvm stdin' ]]
  printf 'MICROVM_STDIN_OK\n'
fi
printf 'MICROVM_BOUNDARY_OK=%s|%s\n' "$1" "$2"
INNER
  chmod +x "$workspace/assert-microvm.sh"

  command=(
    "$sandbox/bin/wrix" --profile-config "$profile_config" run "$workspace" --
    /bin/bash /workspace/assert-microvm.sh alpha "two words"
  )
  printf -v command_line '%q ' "${command[@]}"
  output=$(run_microvm_probe "$command_line") || return

  if [[ "$output" != *"MICROVM_BOUNDARY_OK=alpha|two words"* ]]; then
    fail "live krun microVM did not preserve the private Nix execution contract: $output"
    return 1
  fi
  spawn_config="$TEST_TMP/spawn.json"
  wrix_write_spawn_config "$spawn_config" "$workspace" /bin/bash /workspace/assert-microvm.sh alpha "two words"
  output=$(printf 'microvm stdin\n' | WRIX_MICROVM=1 wrix_run_spawn "$sandbox" "$profile_config" "$spawn_config" --stdio)
  [[ "$output" == *MICROVM_STDIN_OK* && "$output" == *'MICROVM_BOUNDARY_OK=alpha|two words'* ]] \
    || fail "microVM spawn lost stdin or arguments: $output"

  wrix_write_spawn_config "$spawn_config" "$workspace" /bin/bash -c 'printf "MICROVM_FAILURE\n"; exit 37'
  status=0
  output=$(WRIX_MICROVM=1 wrix_run_spawn "$sandbox" "$profile_config" "$spawn_config") || status=$?
  [[ "$status" == 37 && "$output" == *MICROVM_FAILURE* ]] \
    || fail "microVM spawn lost guest failure status/output: $status $output"
  printf 'PASS: live krun run/spawn preserve identity, private Nix, stdin, arguments and exit status\n'
}

case "${1:-}" in
  ""|test_linux_microvm_runtime)
    test_microvm_probe_reports_launch_failure
    test_linux_microvm_runtime
    ;;
  test_microvm_probe_reports_launch_failure) test_microvm_probe_reports_launch_failure ;;
  *) fail "unknown microVM verifier: $1"; exit 64 ;;
esac
