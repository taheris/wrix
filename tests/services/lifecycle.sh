#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-lifecycle.XXXXXX)"
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
  cargo build --quiet -p wrix-cli --bin wrix
  printf '%s\n' "$REPO_ROOT/target/debug/wrix"
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

log_call() {
  printf '%s\n' "$*" >>"$STATE_DIR/calls"
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
    name="${@: -1}"
    if [[ ! -f "$(state_file "$name")" ]]; then
      exit 1
    fi
    if [[ "$*" == *'{{.State.Running}}'* ]]; then
      printf 'true\n'
    else
      printf 'running\n'
    fi
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
    log_call "$*"
    ;;
  rm)
    name="${@: -1}"
    rm -f "$(state_file "$name")"
    log_call "$*"
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

assert_not_equals() {
  local label="$1"
  local first="$2"
  local second="$3"
  if [[ "$first" == "$second" ]]; then
    fail "$label: both values were '$first'"
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

assert_file_contains() {
  local label="$1"
  local file="$2"
  local needle="$3"
  if ! grep -F -- "$needle" "$file" >/dev/null; then
    fail "$label: missing '$needle' in $file"
  fi
}

with_fake_runtime_env() {
  local runtime_dir="$TEST_TMP/runtime-bin"
  local state_dir="$TEST_TMP/runtime-state"
  mkdir -p "$runtime_dir" "$state_dir"
  write_fake_runtime "$runtime_dir/podman"
  export WRIX_CONTAINER_RUNTIME="$runtime_dir/podman"
  export WRIX_FAKE_RUNTIME_STATE="$state_dir"
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

test_workspace_identity() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local first_ws="$TEST_TMP/repo-one"
  local second_ws="$TEST_TMP/repo-two"
  mkdir -p "$first_ws" "$second_ws"

  (cd "$first_ws" && "$wrix_bin" service start >"$TEST_TMP/first-start.txt")
  (cd "$first_ws" && "$wrix_bin" service endpoints >"$TEST_TMP/first-endpoints.json")
  (cd "$first_ws" && "$wrix_bin" service start >"$TEST_TMP/first-start-again.txt")
  (cd "$first_ws" && "$wrix_bin" service endpoints >"$TEST_TMP/first-endpoints-again.json")
  (cd "$second_ws" && "$wrix_bin" service start >"$TEST_TMP/second-start.txt")
  (cd "$second_ws" && "$wrix_bin" service endpoints >"$TEST_TMP/second-endpoints.json")

  local first_name first_name_again second_name
  first_name="$(json_get "$TEST_TMP/first-endpoints.json" container_name)"
  first_name_again="$(json_get "$TEST_TMP/first-endpoints-again.json" container_name)"
  second_name="$(json_get "$TEST_TMP/second-endpoints.json" container_name)"
  assert_equals "first container name" "repo-one-service" "$first_name"
  assert_equals "stable container name" "$first_name" "$first_name_again"
  assert_equals "second container name" "repo-two-service" "$second_name"

  local first_hash first_hash_again second_hash
  first_hash="$(json_get "$TEST_TMP/first-endpoints.json" workspace_hash)"
  first_hash_again="$(json_get "$TEST_TMP/first-endpoints-again.json" workspace_hash)"
  second_hash="$(json_get "$TEST_TMP/second-endpoints.json" workspace_hash)"
  assert_equals "stable workspace hash" "$first_hash" "$first_hash_again"
  assert_not_equals "different checkout hash" "$first_hash" "$second_hash"

  local first_state first_state_again second_state first_cache second_cache first_port second_port
  first_state="$(json_get "$TEST_TMP/first-endpoints.json" state_root)"
  first_state_again="$(json_get "$TEST_TMP/first-endpoints-again.json" state_root)"
  second_state="$(json_get "$TEST_TMP/second-endpoints.json" state_root)"
  first_cache="$(json_get "$TEST_TMP/first-endpoints.json" cache_root)"
  second_cache="$(json_get "$TEST_TMP/second-endpoints.json" cache_root)"
  first_port="$(json_get "$TEST_TMP/first-endpoints.json" endpoints.cache_http.port)"
  second_port="$(json_get "$TEST_TMP/second-endpoints.json" endpoints.cache_http.port)"
  assert_equals "stable state root" "$first_state" "$first_state_again"
  assert_not_equals "different state roots" "$first_state" "$second_state"
  assert_not_equals "different cache roots" "$first_cache" "$second_cache"
  assert_not_equals "different cache ports" "$first_port" "$second_port"
  assert_port_range "first cache port" "$first_port" 21000 22999
  assert_port_range "second cache port" "$second_port" 21000 22999

  assert_file_contains "detached run" "$WRIX_FAKE_RUNTIME_STATE/run-repo-one-service" "run -d --name repo-one-service"
  assert_file_contains "workspace hash label" "$WRIX_FAKE_RUNTIME_STATE/run-repo-one-service" "wrix.workspace.hash=$first_hash"
}

test_devshell_start_is_independent() {
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-devshell"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-devshell"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-devshell"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/devshell-repo"
  mkdir -p "$workspace"
  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/devshell-start.txt")

  "$WRIX_CONTAINER_RUNTIME" container exists devshell-repo-service
  if grep -F -- 'rm -f devshell-repo-service' "$WRIX_FAKE_RUNTIME_STATE/calls" >/dev/null; then
    fail "service start removed the container during shell exit simulation"
  fi
  assert_file_contains "detached devshell run" "$WRIX_FAKE_RUNTIME_STATE/run-devshell-repo-service" "run -d --name devshell-repo-service"

  local no_cache_workspace="$TEST_TMP/no-cache-repo"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-no-cache"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-no-cache"
  mkdir -p "$no_cache_workspace" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  (cd "$no_cache_workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/no-cache-start.txt")
  if [[ -d "$XDG_STATE_HOME/wrix/workspaces" ]]; then
    local cache_files
    cache_files="$(find "$XDG_STATE_HOME/wrix/workspaces" -name cache.lock -print)"
    if [[ -n "$cache_files" ]]; then
      fail "--no-cache created cache state without beads: $cache_files"
    fi
  fi
}

ALL_TESTS=(
  test_fake_runtime_contract
  test_workspace_identity
  test_devshell_start_is_independent
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
