#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-cache-state.XXXXXX)"
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
  cargo build --quiet -p wrix-cli --bin wrix || return 1
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

run_file() {
  local name="$1"
  printf '%s/run-%s\n' "$STATE_DIR" "$name"
}

port_lines_for_args() {
  local run_args="$1"
  local previous=""
  local token
  local mapping
  local rest
  local host_port
  local container_port
  local -a tokens=()
  read -r -a tokens <<<"$run_args"
  for token in "${tokens[@]}"; do
    if [[ "$previous" == "-p" || "$previous" == "--publish" ]]; then
      mapping="$token"
      case "$mapping" in
        127.0.0.1:*:*)
          rest="${mapping#127.0.0.1:}"
          host_port="${rest%%:*}"
          container_port="${rest##*:}"
          printf '%s/tcp -> 127.0.0.1:%s\n' "$container_port" "$host_port"
          ;;
      esac
    fi
    previous="$token"
  done
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
    printf '%s\n' "$*" >"$(run_file "$name")"
    ;;
  port)
    name="${2:-}"
    if [[ ! -f "$(state_file "$name")" ]]; then
      printf 'Error: no such object: "%s"\n' "$name" >&2
      exit 1
    fi
    run_path="$(run_file "$name")"
    if [[ -f "$run_path" ]]; then
      port_lines_for_args "$(<"$run_path")"
    fi
    ;;
  rm)
    name="${@: -1}"
    rm -f "$(state_file "$name")" "$(run_file "$name")"
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

assert_path_exists() {
  local path="$1"
  if [[ ! -e "$path" ]]; then
    fail "expected path to exist: $path"
  fi
}

assert_dir_exists() {
  local path="$1"
  if [[ ! -d "$path" ]]; then
    fail "expected directory to exist: $path"
  fi
}

assert_not_under_workspace() {
  local label="$1"
  local path="$2"
  local workspace="$3"
  case "$path" in
    "$workspace" | "$workspace"/*) fail "$label lives under workspace: $path" ;;
  esac
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

test_fake_runtime_contract() {
  with_fake_runtime_env
  "$WRIX_CONTAINER_RUNTIME" container exists demo && fail "demo should not exist before run"
  "$WRIX_CONTAINER_RUNTIME" run -d --name demo image sh -c 'sleep infinity'
  "$WRIX_CONTAINER_RUNTIME" container exists demo
  local running
  running="$($WRIX_CONTAINER_RUNTIME inspect --format '{{.State.Running}}' demo)"
  if [[ "$running" != "true" ]]; then
    fail "inspect did not report a running fake container"
  fi
  "$WRIX_CONTAINER_RUNTIME" rm -f demo
  if "$WRIX_CONTAINER_RUNTIME" container exists demo; then
    fail "demo should be removed"
  fi
}

test_state_layout() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/cache-workspace"
  mkdir -p "$workspace"
  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/start.txt")
  (cd "$workspace" && "$wrix_bin" service endpoints >"$TEST_TMP/endpoints.json")

  local state_root cache_root cache_port
  state_root="$(json_get "$TEST_TMP/endpoints.json" state_root)"
  cache_root="$(json_get "$TEST_TMP/endpoints.json" cache_root)"
  cache_port="$(json_get "$TEST_TMP/endpoints.json" endpoints.cache_http.port)"

  assert_not_under_workspace "state root" "$state_root" "$workspace"
  assert_not_under_workspace "cache root" "$cache_root" "$workspace"
  assert_dir_exists "$state_root"
  assert_dir_exists "$cache_root"
  assert_path_exists "$state_root/cache.lock"
  assert_path_exists "$state_root/cache-status.json"
  assert_dir_exists "$state_root/gcroots"
  assert_dir_exists "$state_root/keys"
  assert_path_exists "$state_root/keys/cache.secret"
  assert_path_exists "$state_root/keys/cache.pub"
  assert_dir_exists "$state_root/pending"
  assert_path_exists "$state_root/publish-roots.json"
  assert_path_exists "$state_root/services.json"
  assert_path_exists "$cache_root/nix-cache-info"
  assert_dir_exists "$cache_root/nar"
  assert_dir_exists "$cache_root/log"

  if (( cache_port < 21000 || cache_port > 22999 )); then
    fail "cache port $cache_port is outside 21000-22999"
  fi

  local no_cache_workspace="$TEST_TMP/cache-disabled-workspace"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-disabled"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-disabled"
  mkdir -p "$no_cache_workspace/.beads/dolt" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  (cd "$no_cache_workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/no-cache-start.txt")
  (cd "$no_cache_workspace" && "$wrix_bin" service endpoints --no-cache >"$TEST_TMP/no-cache-endpoints.json")

  local disabled_state disabled_cache_endpoint
  disabled_state="$(json_get "$TEST_TMP/no-cache-endpoints.json" state_root)"
  disabled_cache_endpoint="$(json_get "$TEST_TMP/no-cache-endpoints.json" endpoints.cache_http)"
  if [[ "$disabled_cache_endpoint" != "None" ]]; then
    fail "--no-cache published a cache endpoint: $disabled_cache_endpoint"
  fi
  if [[ -e "$disabled_state/cache.lock" || -e "$disabled_state/keys/cache.secret" ]]; then
    fail "--no-cache created cache-only state under $disabled_state"
  fi
}

test_invalid_public_key_regenerated() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-invalid-key"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-invalid-key"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-invalid-key"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/invalid-key-workspace"
  mkdir -p "$workspace"
  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/invalid-key-start.txt")
  (cd "$workspace" && "$wrix_bin" service endpoints >"$TEST_TMP/invalid-key-endpoints.json")

  local state_root invalid_key regenerated_key
  state_root="$(json_get "$TEST_TMP/invalid-key-endpoints.json" state_root)"
  invalid_key="wrix-cache:be619e8138e924f7"
  printf '%s\n' "$invalid_key" >"$state_root/keys/cache.pub"

  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/invalid-key-restart.txt")
  regenerated_key="$(<"$state_root/keys/cache.pub")"
  if [[ "$regenerated_key" == "$invalid_key" ]]; then
    fail "service start left invalid project cache public key in place"
  fi
  python3 - "$regenerated_key" <<'PY'
import base64
import sys
name, payload = sys.argv[1].strip().split(':', 1)
if not name or len(base64.b64decode(payload)) != 32:
    raise SystemExit(1)
PY
}

ALL_TESTS=(
  test_fake_runtime_contract
  test_state_layout
  test_invalid_public_key_regenerated
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
