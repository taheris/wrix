#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-beads-shellhook.XXXXXX)"
SOCKET_PIDS=()
cleanup() {
  local pid
  for pid in "${SOCKET_PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true # best-effort: socket helper may already be gone.
  done
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

require_command() {
  local command_name="$1"
  if ! command -v "$command_name" >/dev/null 2>&1; then
    printf 'SKIP: %s is required\n' "$command_name" >&2
    exit 77
  fi
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle' in: $haystack"
  fi
}

assert_not_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    fail "$label: unexpected '$needle' in: $haystack"
  fi
}

assert_file_contains() {
  local label="$1"
  local file="$2"
  local needle="$3"
  if ! grep -F -- "$needle" "$file" >/dev/null; then
    fail "$label: missing '$needle' in $file"
  fi
}

assert_file_not_contains() {
  local label="$1"
  local file="$2"
  local needle="$3"
  if [[ -f "$file" ]] && grep -F -- "$needle" "$file" >/dev/null; then
    fail "$label: unexpected '$needle' in $file"
  fi
}

require_shellhook_deps() {
  require_command nix
  require_command jq
  require_command python3
}

write_linux_shellhook() {
  local out_file="$1"
  local jq_out
  jq_out="$(dirname "$(dirname "$(command -v jq)")")"
  nix eval --impure --raw --expr "
    let
      pkgs = {
        stdenv = { isDarwin = false; };
        jq = { outPath = \"$jq_out\"; };
      };
    in (import $REPO_ROOT/lib/beads/default.nix { inherit pkgs; wrix = null; }).shellHook
  " >"$out_file"
}

write_fake_wrix() {
  local bin_dir="$1"
  mkdir -p "$bin_dir"
  cat >"$bin_dir/wrix" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'wrix %s\n' "$*" >>"${WRIX_FAKE_LOG:?}"
case "$*" in
  "service start --no-cache")
    exit "${WRIX_FAKE_START_RC:-0}"
    ;;
  "service endpoints --no-cache")
    printf '%s\n' "${WRIX_FAKE_ENDPOINTS:?}"
    ;;
  *)
    printf 'unexpected wrix invocation: %s\n' "$*" >&2
    exit 64
    ;;
esac
EOF
  chmod +x "$bin_dir/wrix"
}

write_fake_runtime() {
  local bin_dir="$1"
  cat >"$bin_dir/podman" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'podman %s\n' "$*" >>"${WRIX_FAKE_LOG:?}"
EOF
  chmod +x "$bin_dir/podman"
}

write_fake_systemd() {
  local bin_dir="$1"
  cat >"$bin_dir/systemctl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'systemctl %s\n' "$*" >>"${WRIX_FAKE_LOG:?}"
if [[ "$*" == "--user is-active dbus.service" ]]; then
  exit "${WRIX_FAKE_SYSTEMD_ACTIVE:-0}"
fi
exit 64
EOF
  chmod +x "$bin_dir/systemctl"

  cat >"$bin_dir/systemd-run" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'systemd-run %s\n' "$*" >>"${WRIX_FAKE_LOG:?}"
while [[ "$#" -gt 0 && "$1" != "--" ]]; do
  shift
done
if [[ "${1:-}" == "--" ]]; then
  shift
fi
exec "$@"
EOF
  chmod +x "$bin_dir/systemd-run"
}

write_fast_sleep() {
  local bin_dir="$1"
  cat >"$bin_dir/sleep" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exit 0
EOF
  chmod +x "$bin_dir/sleep"
}

prepare_beads_workspace() {
  local workspace="$1"
  mkdir -p "$workspace/.beads/dolt" "$workspace/.wrix"
}

start_unix_socket() {
  local socket_path="$1"
  rm -f "$socket_path"
  python3 - "$socket_path" <<'PY' &
import socket
import sys
import time

path = sys.argv[1]
server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen(1)
time.sleep(60)
PY
  local pid="$!"
  SOCKET_PIDS+=("$pid")
  local attempt
  for ((attempt = 0; attempt < 100; attempt++)); do
    if [[ -S "$socket_path" ]]; then
      return 0
    fi
    sleep 0.05
  done
  fail "socket helper did not create $socket_path"
}

json_unix_endpoint() {
  local socket_path="$1"
  jq -n --arg socket "$socket_path" '{endpoints:{dolt:{transport:"unix",socket:$socket}}}'
}

run_hook_with_env() {
  local workspace="$1"
  local hook_file="$2"
  local stdout_file="$3"
  local stderr_file="$4"
  shift 4
  (
    cd "$workspace"
    env "$@" bash -euo pipefail -s "$hook_file" <<'HOOK'
. "$1"
printf "AUTO=%s\n" "${BEADS_DOLT_AUTO_START:-}"
printf "SOCKET=%s\n" "${BEADS_DOLT_SERVER_SOCKET:-}"
printf "HOST=%s\n" "${BEADS_DOLT_SERVER_HOST:-}"
printf "PORT=%s\n" "${BEADS_DOLT_SERVER_PORT:-}"
HOOK
  ) >"$stdout_file" 2>"$stderr_file"
}

test_fake_shellhook_tools_contract() {
  require_command jq
  local bin_dir="$TEST_TMP/fake-tools-bin"
  local log_file="$TEST_TMP/fake-tools.log"
  local endpoint_file="$TEST_TMP/fake-tools-endpoints.json"
  write_fake_wrix "$bin_dir"
  write_fake_systemd "$bin_dir"

  WRIX_FAKE_LOG="$log_file" \
    WRIX_FAKE_START_RC=0 \
    "$bin_dir/systemd-run" --user --scope --quiet --collect -- \
      "$bin_dir/wrix" service start --no-cache
  WRIX_FAKE_LOG="$log_file" \
    WRIX_FAKE_ENDPOINTS="$(json_unix_endpoint "$TEST_TMP/fake.sock")" \
    "$bin_dir/wrix" service endpoints --no-cache >"$endpoint_file"

  assert_file_contains "fake systemd-run logs wrapper" "$log_file" "systemd-run --user --scope --quiet --collect -- $bin_dir/wrix service start --no-cache"
  assert_file_contains "fake wrix logs start" "$log_file" "wrix service start --no-cache"
  assert_contains "fake endpoints" "$(<"$endpoint_file")" "\"transport\": \"unix\""
}

test_shellhook_lifecycle_isolation() {
  require_shellhook_deps
  local hook_file="$TEST_TMP/shellHook-linux.sh"
  write_linux_shellhook "$hook_file"

  local workspace="$TEST_TMP/systemd-repo"
  local bin_dir="$TEST_TMP/systemd-bin"
  local log_file="$TEST_TMP/systemd.log"
  local stdout_file="$TEST_TMP/systemd.out"
  local stderr_file="$TEST_TMP/systemd.err"
  prepare_beads_workspace "$workspace"
  start_unix_socket "$workspace/.wrix/dolt.sock"
  write_fake_wrix "$bin_dir"
  write_fake_runtime "$bin_dir"
  write_fake_systemd "$bin_dir"

  run_hook_with_env "$workspace" "$hook_file" "$stdout_file" "$stderr_file" \
    PATH="$bin_dir:$PATH" \
    WRIX_BIN="$bin_dir/wrix" \
    WRIX_CONTAINER_RUNTIME=podman \
    WRIX_FAKE_LOG="$log_file" \
    WRIX_FAKE_SYSTEMD_ACTIVE=0 \
    WRIX_FAKE_ENDPOINTS="$(json_unix_endpoint "$workspace/.wrix/dolt.sock")"

  assert_file_contains "systemd start" "$log_file" "systemd-run --user --scope --quiet --collect -- $bin_dir/wrix service start --no-cache"
  assert_file_contains "public service start" "$log_file" "wrix service start --no-cache"
  assert_file_contains "public service endpoints" "$log_file" "wrix service endpoints --no-cache"
  assert_file_not_contains "legacy helper" "$log_file" "beads-dolt"
  assert_contains "systemd env" "$(<"$stdout_file")" "AUTO=0"
  assert_contains "systemd env" "$(<"$stdout_file")" "SOCKET=$workspace/.wrix/dolt.sock"

  local fallback_workspace="$TEST_TMP/fallback-repo"
  local fallback_bin="$TEST_TMP/fallback-bin"
  local fallback_log="$TEST_TMP/fallback.log"
  local fallback_out="$TEST_TMP/fallback.out"
  local fallback_err="$TEST_TMP/fallback.err"
  prepare_beads_workspace "$fallback_workspace"
  start_unix_socket "$fallback_workspace/.wrix/dolt.sock"
  write_fake_wrix "$fallback_bin"
  write_fake_runtime "$fallback_bin"
  write_fake_systemd "$fallback_bin"

  run_hook_with_env "$fallback_workspace" "$hook_file" "$fallback_out" "$fallback_err" \
    PATH="$fallback_bin:$PATH" \
    WRIX_BIN="$fallback_bin/wrix" \
    WRIX_CONTAINER_RUNTIME=podman \
    WRIX_FAKE_LOG="$fallback_log" \
    WRIX_FAKE_SYSTEMD_ACTIVE=3 \
    WRIX_FAKE_ENDPOINTS="$(json_unix_endpoint "$fallback_workspace/.wrix/dolt.sock")"

  assert_file_not_contains "fallback bypasses systemd" "$fallback_log" "systemd-run"
  assert_file_contains "fallback public service start" "$fallback_log" "wrix service start --no-cache"
  assert_contains "fallback env" "$(<"$fallback_out")" "AUTO=0"
  assert_contains "fallback env" "$(<"$fallback_out")" "SOCKET=$fallback_workspace/.wrix/dolt.sock"
}

test_shellhook_fail_loud() {
  require_shellhook_deps
  local hook_file="$TEST_TMP/shellHook-fail-linux.sh"
  write_linux_shellhook "$hook_file"

  local runtime_workspace="$TEST_TMP/missing-runtime-repo"
  local runtime_bin="$TEST_TMP/missing-runtime-bin"
  local runtime_log="$TEST_TMP/missing-runtime.log"
  local runtime_out="$TEST_TMP/missing-runtime.out"
  local runtime_err="$TEST_TMP/missing-runtime.err"
  local rc
  prepare_beads_workspace "$runtime_workspace"
  write_fake_wrix "$runtime_bin"
  set +e
  run_hook_with_env "$runtime_workspace" "$hook_file" "$runtime_out" "$runtime_err" \
    PATH="$runtime_bin:$PATH" \
    WRIX_BIN="$runtime_bin/wrix" \
    WRIX_CONTAINER_RUNTIME=wrix-missing-runtime \
    WRIX_FAKE_LOG="$runtime_log" \
    WRIX_FAKE_ENDPOINTS="$(json_unix_endpoint "$runtime_workspace/.wrix/dolt.sock")"
  rc="$?"
  set -e
  if [[ "$rc" == "0" ]]; then
    fail "shellHook succeeded without a container runtime"
  fi
  assert_contains "missing runtime" "$(<"$runtime_err")" "service runtime 'wrix-missing-runtime' is not on PATH"
  if [[ -f "$runtime_log" ]]; then
    assert_not_contains "missing runtime did not start" "$(<"$runtime_log")" "wrix service start"
  fi

  local endpoint_workspace="$TEST_TMP/unreachable-endpoint-repo"
  local endpoint_bin="$TEST_TMP/unreachable-endpoint-bin"
  local endpoint_log="$TEST_TMP/unreachable-endpoint.log"
  local endpoint_out="$TEST_TMP/unreachable-endpoint.out"
  local endpoint_err="$TEST_TMP/unreachable-endpoint.err"
  prepare_beads_workspace "$endpoint_workspace"
  write_fake_wrix "$endpoint_bin"
  write_fake_runtime "$endpoint_bin"
  write_fake_systemd "$endpoint_bin"
  write_fast_sleep "$endpoint_bin"
  set +e
  run_hook_with_env "$endpoint_workspace" "$hook_file" "$endpoint_out" "$endpoint_err" \
    PATH="$endpoint_bin:$PATH" \
    WRIX_BIN="$endpoint_bin/wrix" \
    WRIX_CONTAINER_RUNTIME=podman \
    WRIX_FAKE_LOG="$endpoint_log" \
    WRIX_FAKE_SYSTEMD_ACTIVE=3 \
    WRIX_FAKE_ENDPOINTS="$(json_unix_endpoint "$endpoint_workspace/.wrix/missing.sock")"
  rc="$?"
  set -e
  if [[ "$rc" == "0" ]]; then
    fail "shellHook succeeded with an unreachable Dolt endpoint"
  fi
  assert_file_contains "unreachable started service" "$endpoint_log" "wrix service start --no-cache"
  assert_file_contains "unreachable read endpoints" "$endpoint_log" "wrix service endpoints --no-cache"
  assert_contains "unreachable endpoint" "$(<"$endpoint_err")" "Dolt socket did not appear"
  assert_contains "unreachable endpoint" "$(<"$endpoint_err")" "refusing embedded Dolt fallback"
}

ALL_TESTS=(
  test_fake_shellhook_tools_contract
  test_shellhook_lifecycle_isolation
  test_shellhook_fail_loud
)

run_all() {
  local failed=0
  local fn
  for fn in "${ALL_TESTS[@]}"; do
    printf '=== %s ===\n' "$fn"
    if "$fn"; then
      printf 'PASS: %s\n' "$fn"
    else
      printf 'FAIL: %s\n' "$fn" >&2
      failed=$((failed + 1))
    fi
  done
  if [[ "$failed" -ne 0 ]]; then
    printf '%s test(s) failed\n' "$failed" >&2
    return 1
  fi
}

if [[ "$#" -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    printf 'Unknown function: %s\n' "$fn" >&2
    exit 1
  fi
  "$fn"
fi
