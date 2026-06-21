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

require_nix() {
  if ! command -v nix >/dev/null 2>&1; then
    printf 'SKIP: nix is required for image label assertions\n' >&2
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

image_file() {
  local name="$1"
  name="${name//\//_}"
  name="${name//:/_}"
  printf '%s/%s.image\n' "$STATE_DIR" "$name"
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
  image)
    if [[ "${2:-}" == "exists" ]]; then
      if [[ -f "$(image_file "${3:-}")" ]]; then
        exit 0
      fi
      exit 1
    fi
    ;;
  load)
    image="localhost/wrix-service:latest"
    previous=""
    source=""
    for arg in "$@"; do
      if [[ "$previous" == "--input" ]]; then
        source="$arg"
      fi
      previous="$arg"
    done
    printf 'loaded from %s\n' "$source" >"$(image_file "$image")"
    log_call "$*"
    ;;
  inspect)
    name="${@: -1}"
    if [[ ! -f "$(state_file "$name")" ]]; then
      exit 1
    fi
    if [[ "$*" == *'{{.State.Running}}'* ]]; then
      printf 'true\n'
    elif [[ "$*" == *'index .Config.Labels "wrix.workspace"'* ]]; then
      workspace="<no value>"
      if [[ -f "$STATE_DIR/run-$name" ]]; then
        run_args="$(<"$STATE_DIR/run-$name")"
        for token in $run_args; do
          case "$token" in
            wrix.workspace=*) workspace="${token#wrix.workspace=}" ;;
          esac
        done
      fi
      printf '%s\n' "$workspace"
    else
      printf 'running\n'
    fi
    ;;
  ps)
    for run_file in "$STATE_DIR"/run-*; do
      [[ -e "$run_file" ]] || continue
      run_args="$(<"$run_file")"
      if [[ "$run_args" == *'wrix.kind=service'* ]]; then
        name="${run_file##*/run-}"
        printf '%s\n' "$name"
      fi
    done
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
    rm -f "$(state_file "$name")" "$STATE_DIR/run-$name"
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

assert_sha256_hex() {
  local label="$1"
  local value="$2"
  if [[ ! "$value" =~ ^[0-9a-f]{64}$ ]]; then
    fail "$label: expected lowercase sha256 hex, got '$value'"
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

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle'"
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
  export WRIX_SERVICE_ALLOW_TEMP_CACHE=1
  unset WRIX_SERVICE_IMAGE WRIX_SERVICE_IMAGE_SOURCE
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
  assert_sha256_hex "first workspace hash" "$first_hash"
  assert_sha256_hex "second workspace hash" "$second_hash"
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
  assert_contains "state root uses workspace hash" "$first_state" "$first_hash"
  assert_contains "cache root uses workspace hash" "$first_cache" "$first_hash"
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

test_temp_cache_only_workspace_does_not_start_service() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env
  unset WRIX_SERVICE_ALLOW_TEMP_CACHE

  export HOME="$TEST_TMP/home-temp-cache-only"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-temp-cache-only"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-temp-cache-only"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/temp-cache-only-workspace"
  mkdir -p "$workspace"
  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/temp-cache-only-start.txt")
  (cd "$workspace" && "$wrix_bin" service endpoints >"$TEST_TMP/temp-cache-only-endpoints.json")

  if "$WRIX_CONTAINER_RUNTIME" container exists temp-cache-only-workspace-service; then
    fail "cache-only temp workspace started a persistent service container"
  fi
  local cache_endpoint
  cache_endpoint="$(json_get "$TEST_TMP/temp-cache-only-endpoints.json" endpoints.cache_http)"
  assert_equals "temp cache endpoint" "None" "$cache_endpoint"
}

test_loom_bead_workspace_uses_repo_service() {
  require_python
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-loom-bead"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-loom-bead"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-loom-bead"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local repo="$TEST_TMP/loom-repo"
  local bead="$repo/.loom/beads/lm-gzgw.3"
  local repo_real
  mkdir -p "$repo/.git" "$bead/.git"
  repo_real="$(cd "$repo" && pwd -P)"
  (cd "$bead" && "$wrix_bin" service start >"$TEST_TMP/loom-bead-start.txt")
  (cd "$bead" && "$wrix_bin" service endpoints >"$TEST_TMP/loom-bead-endpoints.json")
  (cd "$repo" && "$wrix_bin" service endpoints >"$TEST_TMP/loom-repo-endpoints.json")

  local bead_hash repo_hash bead_port repo_port
  bead_hash="$(json_get "$TEST_TMP/loom-bead-endpoints.json" workspace_hash)"
  repo_hash="$(json_get "$TEST_TMP/loom-repo-endpoints.json" workspace_hash)"
  bead_port="$(json_get "$TEST_TMP/loom-bead-endpoints.json" endpoints.cache_http.port)"
  repo_port="$(json_get "$TEST_TMP/loom-repo-endpoints.json" endpoints.cache_http.port)"
  assert_sha256_hex "loom bead workspace hash" "$bead_hash"
  assert_equals "loom bead outer hash" "$repo_hash" "$bead_hash"
  assert_equals "loom bead outer cache port" "$repo_port" "$bead_port"

  if ! "$WRIX_CONTAINER_RUNTIME" container exists loom-repo-service; then
    fail "loom bead workspace did not start the repository service"
  fi
  if "$WRIX_CONTAINER_RUNTIME" container exists lm-gzgw.3-service; then
    fail "loom bead workspace started a bead-clone service container"
  fi
  assert_equals \
    "loom bead container name" \
    "loom-repo-service" \
    "$(json_get "$TEST_TMP/loom-bead-endpoints.json" container_name)"
  assert_equals \
    "loom bead workspace path" \
    "$repo_real" \
    "$(json_get "$TEST_TMP/loom-bead-endpoints.json" workspace_path)"
}

test_service_start_loads_image_source() {
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-image-source"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-image-source"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-image-source"
  export WRIX_SERVICE_IMAGE_SOURCE="$TEST_TMP/wrix-service.tar.gz"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
  printf 'image archive\n' >"$WRIX_SERVICE_IMAGE_SOURCE"

  local workspace="$TEST_TMP/image-source-repo"
  mkdir -p "$workspace"
  (cd "$workspace" && "$wrix_bin" service start >"$TEST_TMP/image-source-start.txt")

  assert_file_contains "service image load" "$WRIX_FAKE_RUNTIME_STATE/calls" "load --input $WRIX_SERVICE_IMAGE_SOURCE"
  assert_file_contains "service run after load" "$WRIX_FAKE_RUNTIME_STATE/run-image-source-repo-service" "localhost/wrix-service:latest"
}

test_service_image_labels() {
  require_python
  require_nix

  local labels_json
  labels_json="$(nix eval --no-warn-dirty --json "$REPO_ROOT#wrix-service-image.labels")"
  python3 - "$labels_json" <<'PY'
import json
import sys
labels = json.loads(sys.argv[1])
expected = {
    "wrix.managed": "true",
    "wrix.image.kind": "service",
}
for key, value in expected.items():
    actual = labels.get(key)
    if actual != value:
        print(f"FAIL: service image label {key}={actual!r}, expected {value!r}", file=sys.stderr)
        sys.exit(1)
PY
}

test_service_mounts_beads_worktree_remote() {
  local wrix_bin
  wrix_bin="$(build_wrix)"
  with_fake_runtime_env

  export HOME="$TEST_TMP/home-beads-remote"
  export XDG_STATE_HOME="$TEST_TMP/xdg-state-beads-remote"
  export XDG_CACHE_HOME="$TEST_TMP/xdg-cache-beads-remote"
  mkdir -p "$HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

  local workspace="$TEST_TMP/beads-remote-repo"
  local worktree_remote="$workspace/.git/beads-worktrees/beads/.beads/dolt-remote"
  mkdir -p "$workspace/.beads/dolt" "$worktree_remote"
  (cd "$workspace" && "$wrix_bin" service start --no-cache >"$TEST_TMP/beads-remote-start.txt")

  assert_file_contains \
    "beads remote bind mount" \
    "$WRIX_FAKE_RUNTIME_STATE/run-beads-remote-repo-service" \
    "$worktree_remote:$worktree_remote:rw"
}

ALL_TESTS=(
  test_fake_runtime_contract
  test_workspace_identity
  test_devshell_start_is_independent
  test_temp_cache_only_workspace_does_not_start_service
  test_loom_bead_workspace_uses_repo_service
  test_service_start_loads_image_source
  test_service_image_labels
  test_service_mounts_beads_worktree_remote
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
