#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-dolt.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

require_python() {
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'SKIP: python3 is required for JSON assertions\n' >&2
    exit 77
  fi
}

build_wrix() {
  if command -v cargo >/dev/null 2>&1; then
    cargo build --quiet -p wrix-cli --bin wrix
    printf '%s\n' "$REPO_ROOT/target/debug/wrix"
    return 0
  fi
  if command -v nix >/dev/null 2>&1; then
    local out_link="$TEST_TMP/wrix-package"
    nix build --no-warn-dirty --out-link "$out_link" "$REPO_ROOT#wrix" >/dev/null
    printf '%s\n' "$out_link/bin/wrix"
    return 0
  fi
  printf 'SKIP: cargo or nix is required to build wrix\n' >&2
  exit 77
}

write_fake_runtime() {
  local runtime="$1"
  cat >"$runtime" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

STATE_DIR="${WRIX_FAKE_RUNTIME_STATE:?}"
mkdir -p "$STATE_DIR"

state_file() {
  local name="$1"
  printf '%s/%s.state\n' "$STATE_DIR" "$name"
}

last_arg() {
  local value=""
  local arg
  for arg in "$@"; do
    value="$arg"
  done
  printf '%s\n' "$value"
}

case "${1:-}" in
  container)
    if [[ "${2:-}" == "exists" ]]; then
      if [[ -f "$(state_file "${3:-}")" ]]; then
        exit 0
      fi
      exit 1
    fi
    ;;
  inspect)
    name="$(last_arg "$@")"
    if [[ -f "$(state_file "$name")" ]]; then
      printf 'true\n'
      exit 0
    fi
    exit 1
    ;;
  ps)
    ;;
  run)
    name=""
    previous=""
    for arg in "$@"; do
      if [[ "$previous" == "--name" ]]; then
        name="$arg"
      fi
      previous="$arg"
    done
    if [[ -z "$name" ]]; then
      printf 'missing --name\n' >&2
      exit 2
    fi
    printf 'running\n' >"$(state_file "$name")"
    printf '%s\n' "$*" >"$STATE_DIR/run-$name"
    ;;
  rm)
    name="$(last_arg "$@")"
    rm -f "$(state_file "$name")"
    ;;
  logs)
    printf 'logs for %s\n' "${2:-}"
    ;;
  *)
    printf 'unsupported fake runtime command: %s\n' "$*" >&2
    exit 2
    ;;
esac
EOF
  chmod +x "$runtime"
}

with_fake_runtime_env() {
  local runtime_dir="$TEST_TMP/runtime-bin"
  local state_dir="$TEST_TMP/runtime-state"
  mkdir -p "$runtime_dir" "$state_dir"
  write_fake_runtime "$runtime_dir/podman"
  export WRIX_CONTAINER_RUNTIME="$runtime_dir/podman"
  export WRIX_FAKE_RUNTIME_STATE="$state_dir"
  export WRIX_SERVICE_ALLOW_TEMP_CACHE=1
}

json_get() {
  local file="$1"
  local path="$2"
  python3 - "$file" "$path" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    value = json.load(handle)
for part in sys.argv[2].split('.'):
    value = value[part]
print(value)
PY
}

assert_equals() {
  local label="$1"
  local expected="$2"
  local actual="$3"
  if [[ "$actual" != "$expected" ]]; then
    fail "$label: expected '$expected', got '$actual'"
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

assert_port_range() {
  local label="$1"
  local port="$2"
  local start="$3"
  local end="$4"
  if (( port < start || port > end )); then
    fail "$label: port $port outside $start-$end"
  fi
}

test_fake_runtime_contract() {
  with_fake_runtime_env
  "$WRIX_CONTAINER_RUNTIME" container exists demo && fail "demo should not exist before run"
  "$WRIX_CONTAINER_RUNTIME" run -d --name demo image sh -c 'sleep infinity'
  "$WRIX_CONTAINER_RUNTIME" container exists demo
  assert_equals "running inspect" "true" "$($WRIX_CONTAINER_RUNTIME inspect --format '{{.State.Running}}' demo)"
  "$WRIX_CONTAINER_RUNTIME" rm -f demo
  if "$WRIX_CONTAINER_RUNTIME" container exists demo; then
    fail "demo should be removed"
  fi
}

test_linux_dolt_uses_workspace_socket() {
  require_python
  local wrix_bin workspace socket endpoints run_args status_output socket_output
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-unix"
  export XDG_STATE_HOME="$TEST_TMP/state-unix"
  export XDG_CACHE_HOME="$TEST_TMP/cache-unix"
  unset WRIX_DOLT_TRANSPORT
  workspace="$TEST_TMP/socket-repo"
  mkdir -p "$workspace/.beads/dolt" "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  (cd "$workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/start-unix.txt")
  (cd "$workspace" && "$wrix_bin" service endpoints --no-cache >"$TEST_TMP/endpoints-unix.json")
  endpoints="$TEST_TMP/endpoints-unix.json"
  socket="$workspace/.wrix/dolt.sock"

  assert_equals "dolt transport" "unix" "$(json_get "$endpoints" endpoints.dolt.transport)"
  assert_equals "dolt socket" "$socket" "$(json_get "$endpoints" endpoints.dolt.socket)"
  assert_equals "beads socket env" "$socket" "$(json_get "$endpoints" endpoints.dolt.env.BEADS_DOLT_SERVER_SOCKET)"
  assert_equals "legacy tcp endpoint disabled" "None" "$(json_get "$endpoints" endpoints.dolt_tcp)"

  run_args="$(<"$WRIX_FAKE_RUNTIME_STATE/run-socket-repo-service")"
  assert_contains "dolt data mount" "$run_args" "$workspace/.beads/dolt:/var/lib/wrix/beads/dolt:rw"
  assert_contains "socket directory mount" "$run_args" "$workspace/.wrix:/run/wrix:rw"
  assert_contains "dolt server command" "$run_args" "dolt sql-server"
  assert_contains "socket server option" "$run_args" "--socket /run/wrix/dolt.sock"
  assert_not_contains "no tcp publish on unix" "$run_args" ":3306"
  assert_not_contains "no whole workspace mount" "$run_args" "$workspace:/workspace"

  status_output="$(cd "$workspace" && "$wrix_bin" service dolt status)"
  socket_output="$(cd "$workspace" && "$wrix_bin" service dolt socket)"
  assert_contains "dolt status" "$status_output" "transport: unix"
  assert_equals "dolt socket command" "$socket" "$socket_output"
}

test_workspace_naming_determinism() {
  require_python
  local wrix_bin first_workspace second_workspace first_endpoints first_again_endpoints second_endpoints
  local first_name first_name_again second_name first_hash first_hash_again second_hash
  local first_socket first_socket_again second_socket first_state first_state_again second_state
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-determinism"
  export XDG_STATE_HOME="$TEST_TMP/state-determinism"
  export XDG_CACHE_HOME="$TEST_TMP/cache-determinism"
  export WRIX_DOLT_TRANSPORT="unix"
  first_workspace="$TEST_TMP/deterministic-one"
  second_workspace="$TEST_TMP/deterministic-two"
  mkdir -p \
    "$first_workspace/.beads/dolt" \
    "$second_workspace/.beads/dolt" \
    "$HOME" \
    "$XDG_STATE_HOME" \
    "$XDG_CACHE_HOME"

  (cd "$first_workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/start-deterministic-one.txt")
  (cd "$first_workspace" && "$wrix_bin" service endpoints --no-cache >"$TEST_TMP/endpoints-deterministic-one.json")
  (cd "$first_workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/start-deterministic-one-again.txt")
  (cd "$first_workspace" && "$wrix_bin" service endpoints --no-cache >"$TEST_TMP/endpoints-deterministic-one-again.json")
  (cd "$second_workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/start-deterministic-two.txt")
  (cd "$second_workspace" && "$wrix_bin" service endpoints --no-cache >"$TEST_TMP/endpoints-deterministic-two.json")

  first_endpoints="$TEST_TMP/endpoints-deterministic-one.json"
  first_again_endpoints="$TEST_TMP/endpoints-deterministic-one-again.json"
  second_endpoints="$TEST_TMP/endpoints-deterministic-two.json"
  first_name="$(json_get "$first_endpoints" container_name)"
  first_name_again="$(json_get "$first_again_endpoints" container_name)"
  second_name="$(json_get "$second_endpoints" container_name)"
  first_hash="$(json_get "$first_endpoints" workspace_hash)"
  first_hash_again="$(json_get "$first_again_endpoints" workspace_hash)"
  second_hash="$(json_get "$second_endpoints" workspace_hash)"
  first_socket="$(json_get "$first_endpoints" endpoints.dolt.socket)"
  first_socket_again="$(json_get "$first_again_endpoints" endpoints.dolt.socket)"
  second_socket="$(json_get "$second_endpoints" endpoints.dolt.socket)"
  first_state="$(json_get "$first_endpoints" state_root)"
  first_state_again="$(json_get "$first_again_endpoints" state_root)"
  second_state="$(json_get "$second_endpoints" state_root)"

  assert_equals "stable container name" "$first_name" "$first_name_again"
  assert_equals "stable workspace hash" "$first_hash" "$first_hash_again"
  assert_equals "stable Dolt socket" "$first_socket" "$first_socket_again"
  assert_equals "stable state root" "$first_state" "$first_state_again"
  assert_equals "first container name" "deterministic-one-service" "$first_name"
  assert_equals "second container name" "deterministic-two-service" "$second_name"
  assert_not_contains "first name avoids legacy beads helper" "$first_name" "-beads"
  assert_not_contains "second name avoids legacy beads helper" "$second_name" "-beads"
  if [[ "$first_hash" == "$second_hash" ]]; then
    fail "different checkout paths produced the same workspace hash: $first_hash"
  fi
  if [[ "$first_socket" == "$second_socket" ]]; then
    fail "different checkout paths produced the same Dolt socket: $first_socket"
  fi
  if [[ "$first_state" == "$second_state" ]]; then
    fail "different checkout paths produced the same state root: $first_state"
  fi
}

test_fallback_dolt_uses_loopback_tcp() {
  require_python
  local wrix_bin workspace endpoints port run_args host_output port_output
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-tcp"
  export XDG_STATE_HOME="$TEST_TMP/state-tcp"
  export XDG_CACHE_HOME="$TEST_TMP/cache-tcp"
  export WRIX_DOLT_TRANSPORT="tcp"
  workspace="$TEST_TMP/tcp-repo"
  mkdir -p "$workspace/.beads/dolt" "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  (cd "$workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/start-tcp.txt")
  (cd "$workspace" && "$wrix_bin" service endpoints --no-cache >"$TEST_TMP/endpoints-tcp.json")
  endpoints="$TEST_TMP/endpoints-tcp.json"
  port="$(json_get "$endpoints" endpoints.dolt.port)"

  assert_equals "dolt transport" "tcp" "$(json_get "$endpoints" endpoints.dolt.transport)"
  assert_equals "dolt host" "127.0.0.1" "$(json_get "$endpoints" endpoints.dolt.host)"
  assert_equals "dolt tcp host" "127.0.0.1" "$(json_get "$endpoints" endpoints.dolt_tcp.host)"
  assert_equals "dolt env host" "127.0.0.1" "$(json_get "$endpoints" endpoints.dolt.env.BEADS_DOLT_SERVER_HOST)"
  assert_equals "dolt env port" "$port" "$(json_get "$endpoints" endpoints.dolt.env.BEADS_DOLT_SERVER_PORT)"
  assert_equals "unix endpoint disabled" "None" "$(json_get "$endpoints" endpoints.dolt_unix)"
  assert_port_range "dolt tcp port" "$port" 23000 24999

  run_args="$(<"$WRIX_FAKE_RUNTIME_STATE/run-tcp-repo-service")"
  assert_contains "dolt data mount" "$run_args" "$workspace/.beads/dolt:/var/lib/wrix/beads/dolt:rw"
  assert_contains "tcp publish" "$run_args" "127.0.0.1:$port:3306"
  assert_contains "tcp server option" "$run_args" "--host 0.0.0.0 --port 3306"
  assert_not_contains "no unix socket mount" "$run_args" "$workspace/.wrix:/run/wrix:rw"
  assert_not_contains "no whole workspace mount" "$run_args" "$workspace:/workspace"

  host_output="$(cd "$workspace" && "$wrix_bin" service dolt host)"
  port_output="$(cd "$workspace" && "$wrix_bin" service dolt port)"
  assert_equals "dolt host command" "127.0.0.1" "$host_output"
  assert_equals "dolt port command" "$port" "$port_output"
}

ALL_TESTS=(
  test_fake_runtime_contract
  test_linux_dolt_uses_workspace_socket
  test_workspace_naming_determinism
  test_fallback_dolt_uses_loopback_tcp
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
