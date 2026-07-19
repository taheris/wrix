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
