#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-beads-shellhook.XXXXXX)"
cleanup() {
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

require_shellhook_deps() {
  require_command nix
  require_command jq
  require_command python3
}

write_shellhook() {
  local platform="$1"
  local out_file="$2"
  local is_darwin
  local jq_out
  case "$platform" in
    linux) is_darwin=false ;;
    darwin) is_darwin=true ;;
    *) fail "unknown shellHook platform: $platform" ;;
  esac
  jq_out="$(dirname "$(dirname "$(command -v jq)")")"
  nix eval --impure --raw --expr "
    let
      pkgs = {
        stdenv.hostPlatform.isDarwin = $is_darwin;
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
  "service dolt wait")
    exit "${WRIX_FAKE_DOLT_WAIT_RC:-0}"
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
if [[ "$*" == "--user show-environment" ]]; then
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

json_unix_endpoint() {
  local socket_path="$1"
  jq -n --arg socket "$socket_path" '{endpoints:{dolt:{transport:"unix",socket:$socket}}}'
}

json_tcp_endpoint() {
  local host="$1"
  local port="$2"
  jq -n \
    --arg host "$host" \
    --argjson port "$port" \
    '{endpoints:{dolt:{transport:"tcp",host:$host,port:$port}}}'
}

start_unix_listener() {
  local socket_path="$1"
  python3 - "$socket_path" </dev/null >/dev/null 2>&1 <<'PY' &
import socket
import sys
import time

server = socket.socket(socket.AF_UNIX)
server.bind(sys.argv[1])
server.listen()
time.sleep(30)
PY
  printf '%s\n' "$!"
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

test_shellhook_missing_runtime_fails_loud() {
  require_shellhook_deps
  local platform
  for platform in linux darwin; do
    local hook_file="$TEST_TMP/shellHook-missing-runtime-$platform.sh"
    local workspace="$TEST_TMP/missing-runtime-$platform-repo"
    local bin_dir="$TEST_TMP/missing-runtime-$platform-bin"
    local log_file="$TEST_TMP/missing-runtime-$platform.log"
    local stdout_file="$TEST_TMP/missing-runtime-$platform.out"
    local stderr_file="$TEST_TMP/missing-runtime-$platform.err"
    local rc
    write_shellhook "$platform" "$hook_file"
    prepare_beads_workspace "$workspace"
    write_fake_wrix "$bin_dir"
    set +e
    run_hook_with_env "$workspace" "$hook_file" "$stdout_file" "$stderr_file" \
      PATH="$bin_dir:$PATH" \
      WRIX_BIN="$bin_dir/wrix" \
      WRIX_CONTAINER_RUNTIME=wrix-missing-runtime \
      WRIX_FAKE_LOG="$log_file" \
      WRIX_FAKE_ENDPOINTS="$(json_unix_endpoint "$workspace/.wrix/dolt.sock")"
    rc="$?"
    set -e
    if [[ "$rc" == "0" ]]; then
      fail "$platform shellHook succeeded without a container runtime"
    fi
    if [[ "$platform" == "darwin" ]]; then
      assert_contains "$platform missing runtime" "$(<"$stderr_file")" "no service container runtime is available"
    else
      assert_contains "$platform missing runtime" "$(<"$stderr_file")" "service runtime 'wrix-missing-runtime' is not on PATH"
    fi
    if [[ -f "$log_file" ]]; then
      assert_not_contains "$platform missing runtime did not start" "$(<"$log_file")" "wrix service start"
    fi
  done
}

test_shellhook_unreachable_endpoint_fails_loud() {
  require_shellhook_deps
  local platform
  for platform in linux darwin; do
    local hook_file="$TEST_TMP/shellHook-unreachable-$platform.sh"
    local workspace="$TEST_TMP/unreachable-$platform-repo"
    local bin_dir="$TEST_TMP/unreachable-$platform-bin"
    local log_file="$TEST_TMP/unreachable-$platform.log"
    local stdout_file="$TEST_TMP/unreachable-$platform.out"
    local stderr_file="$TEST_TMP/unreachable-$platform.err"
    local endpoint
    local expected_error
    local rc
    write_shellhook "$platform" "$hook_file"
    prepare_beads_workspace "$workspace"
    write_fake_wrix "$bin_dir"
    write_fake_runtime "$bin_dir"
    write_fake_systemd "$bin_dir"
    write_fast_sleep "$bin_dir"
    if [[ "$platform" == "darwin" ]]; then
      endpoint="$(json_tcp_endpoint "127.0.0.1" 1)"
      expected_error="Dolt TCP endpoint 127.0.0.1:1 is not reachable"
    else
      endpoint="$(json_unix_endpoint "$workspace/.wrix/missing.sock")"
      expected_error="Dolt socket did not appear"
    fi
    set +e
    run_hook_with_env "$workspace" "$hook_file" "$stdout_file" "$stderr_file" \
      PATH="$bin_dir:$PATH" \
      WRIX_BIN="$bin_dir/wrix" \
      WRIX_CONTAINER_RUNTIME=podman \
      WRIX_FAKE_LOG="$log_file" \
      WRIX_FAKE_SYSTEMD_ACTIVE=3 \
      WRIX_FAKE_ENDPOINTS="$endpoint"
    rc="$?"
    set -e
    if [[ "$rc" == "0" ]]; then
      fail "$platform shellHook succeeded with an unreachable Dolt endpoint"
    fi
    assert_file_contains "$platform unreachable started service" "$log_file" "wrix service start --no-cache"
    assert_file_contains "$platform unreachable read endpoints" "$log_file" "wrix service endpoints --no-cache"
    assert_contains "$platform unreachable endpoint" "$(<"$stderr_file")" "$expected_error"
    assert_contains "$platform unreachable endpoint" "$(<"$stderr_file")" "refusing embedded Dolt fallback"
  done
}

test_darwin_shellhook_selects_podman_fallback() {
  require_shellhook_deps
  local hook_file="$TEST_TMP/shellHook-darwin-podman.sh"
  local workspace="$TEST_TMP/darwin-podman-repo"
  local bin_dir="$TEST_TMP/darwin-podman-bin"
  local log_file="$TEST_TMP/darwin-podman.log"
  local stdout_file="$TEST_TMP/darwin-podman.out"
  local stderr_file="$TEST_TMP/darwin-podman.err"
  local socket_path="$workspace/.wrix/dolt.sock"
  local listener_pid
  write_shellhook darwin "$hook_file"
  prepare_beads_workspace "$workspace"
  write_fake_wrix "$bin_dir"
  write_fake_runtime "$bin_dir"
  ln -s "$(command -v bash)" "$bin_dir/bash"
  listener_pid="$(start_unix_listener "$socket_path")"
  while [[ ! -S "$socket_path" ]]; do
    sleep 0.01
  done

  run_hook_with_env "$workspace" "$hook_file" "$stdout_file" "$stderr_file" \
    PATH="$bin_dir" \
    WRIX_BIN="$bin_dir/wrix" \
    WRIX_FAKE_LOG="$log_file" \
    WRIX_FAKE_ENDPOINTS="$(json_unix_endpoint "$socket_path")"
  kill "$listener_pid"
  wait "$listener_pid" 2>/dev/null || true # best-effort: the listener may exit after the explicit kill.

  assert_file_contains "Darwin podman fallback starts service" "$log_file" "wrix service start --no-cache"
  assert_contains "Darwin podman fallback exports socket" "$(<"$stdout_file")" "SOCKET=$socket_path"
}

ALL_TESTS=(
  test_fake_shellhook_tools_contract
  test_shellhook_missing_runtime_fails_loud
  test_shellhook_unreachable_endpoint_fails_loud
  test_darwin_shellhook_selects_podman_fallback
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
