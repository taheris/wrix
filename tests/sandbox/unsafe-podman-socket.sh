#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "skip: linux-only test (uname=$(uname -s))" >&2
  exit 77
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP="$(mktemp -d -t wrix-unsafe-podman.XXXXXX)"
HOST_CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"

cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

command -v jq >/dev/null 2>&1 || {
  echo "skip: jq not on PATH" >&2
  exit 77
}
command -v python3 >/dev/null 2>&1 || {
  echo "skip: python3 not on PATH" >&2
  exit 77
}

PROFILE_CONFIG="$TEST_TMP/profile-config.json"
WORKSPACE="$TEST_TMP/workspace"
HOME_DIR="$TEST_TMP/home"
RUNTIME_DIR="$TEST_TMP/runtime"
STATE_DIR="$TEST_TMP/state"
BIN_DIR="$TEST_TMP/bin"
CACHE_DIR="$TEST_TMP/cache"
LAYOUT="$TEST_TMP/oci-layout"
DESCRIPTOR="$TEST_TMP/image-descriptor.json"
DIGEST="sha256:0000000000000000000000000000000000000000000000000000000000000000"
PODMAN_LOG="$STATE_DIR/podman.log"
mkdir -p "$WORKSPACE" "$HOME_DIR" "$RUNTIME_DIR" "$STATE_DIR" "$BIN_DIR" "$CACHE_DIR" "$LAYOUT"
: >"$PODMAN_LOG"

jq -n \
  --arg digest "$DIGEST" \
  --arg layout "$LAYOUT" \
  '{schema:1,source_kind:"nix-descriptor",digest:$digest,oci_layout:$layout,oci_ref:"latest"}' \
  >"$DESCRIPTOR"
jq -n \
  --arg source "$DESCRIPTOR" \
  --arg digest "$DIGEST" \
  '{schema:1,system:"test",profile:{name:"base",env:{},mounts:[],writable_dirs:[],network_allowlist:[]},image:{ref:"localhost/wrix-unsafe:test",source:$source,source_kind:"nix-descriptor",digest:$digest},agent:{kind:"direct"},resources:{cpus:null,memory_mb:4096,pids_limit:4096},security:{deploy_key:null},network:{default_mode:"open",ipv6:"disabled"},services:{beads:{enable:"auto"},nix_cache:{enable:false}},features:{mcp_runtime:false}}' \
  >"$PROFILE_CONFIG"

cat >"$BIN_DIR/podman" <<'PODMAN'
#!/usr/bin/env bash
set -euo pipefail
state="${WRIX_TEST_STATE:?}"
printf '%s\n' "$*" >>"$state/podman.log"
case "${1:-}" in
  image)
    if [[ "${2:-}" == "inspect" ]]; then
      target="${!#}"
      if [[ "$target" == sha256:* ]]; then
        [[ -f "$state/digest-present" ]] || exit 1
        printf '%s\n' "$target"
      else
        case "${4:-}" in
          '{{.Id}}') printf 'unsafe-image-id\n' ;;
          '{{.Digest}}') printf '%s\n' "${WRIX_TEST_DIGEST:?}" ;;
          '{{ index .Config.Labels "wrix.managed" }}') printf 'true\n' ;;
          *) printf 'unsafe-image-id\n' ;;
        esac
      fi
    fi
    ;;
  images)
    printf 'localhost/wrix-unsafe test unsafe-image-id\n'
    ;;
  info)
    printf 'overlay@%s/graph+%s/run\n' "$state" "$state"
    ;;
  ps)
    ;;
  run)
    : >"$state/container-ran"
    ;;
  tag)
    : >"$state/digest-present"
    ;;
  rmi)
    ;;
esac
PODMAN
chmod +x "$BIN_DIR/podman"

cat >"$BIN_DIR/skopeo" <<'SKOPEO'
#!/usr/bin/env bash
set -euo pipefail
state="${WRIX_TEST_STATE:?}"
printf '%s\n' "$*" >>"$state/skopeo.log"
: >"$state/digest-present"
SKOPEO
chmod +x "$BIN_DIR/skopeo"

run_launcher() {
  local out_file="$1"
  local err_file="$2"
  shift 2
  local rc=0
  local -a launcher
  if [[ -n "${WRIX_TEST_WRIX_BIN:-}" ]]; then
    launcher=("$WRIX_TEST_WRIX_BIN")
  else
    launcher=(cargo run --quiet -p wrix-cli --bin wrix --)
  fi
  (
    cd "$REPO_ROOT"
    env \
      -u WRIX_DEPLOY_KEY \
      -u WRIX_SIGNING_KEY \
      PATH="$BIN_DIR:$PATH" \
      CARGO_HOME="$HOST_CARGO_HOME" \
      HOME="$HOME_DIR" \
      XDG_CACHE_HOME="$CACHE_DIR" \
      XDG_RUNTIME_DIR="$RUNTIME_DIR" \
      WRIX_IMAGE_KEEP_FILE="$STATE_DIR/image-mru.json" \
      WRIX_TEST_DIGEST="$DIGEST" \
      WRIX_TEST_STATE="$STATE_DIR" \
      "$@" \
      "${launcher[@]}" \
      --profile-config "$PROFILE_CONFIG" \
      run "$WORKSPACE" true
  ) >"$out_file" 2>"$err_file" || rc=$?
  printf '%s\n' "$rc"
}

fail() {
  printf '  FAIL: %s\n' "$1" >&2
}

assert_absent() {
  local label="$1"
  local file="$2"
  local needle="$3"
  if grep -qF -- "$needle" "$file"; then
    fail "$label: unexpected '$needle' in $(tr '\n' ' ' < "$file")"
    return 1
  fi
}

assert_contains() {
  local label="$1"
  local file="$2"
  local needle="$3"
  if ! grep -qF -- "$needle" "$file"; then
    fail "$label: expected '$needle'; got $(tr '\n' ' ' < "$file")"
    return 1
  fi
}

test_default_launch_omits_host_podman_socket() {
  local out_file="$TEST_TMP/default.out"
  local err_file="$TEST_TMP/default.err"
  local rc
  rc="$(run_launcher "$out_file" "$err_file")"
  if [[ "$rc" != "0" ]]; then
    fail "default launch exited $rc; stderr=$(tr '\n' ' ' < "$err_file")"
    return 1
  fi
  [[ -f "$STATE_DIR/container-ran" ]] || {
    fail "default launch did not reach podman run"
    return 1
  }
  assert_absent "default launch" "$PODMAN_LOG" "/run/podman/podman.sock" || return 1
  assert_absent "default launch" "$PODMAN_LOG" "CONTAINER_HOST" || return 1
  assert_absent "default launch" "$PODMAN_LOG" "GC_HOST_WORKSPACE" || return 1
  assert_absent "default launch" "$PODMAN_LOG" "GC_HOST_BEADS" || return 1
}

test_legacy_podman_socket_env_is_ignored() {
  local out_file="$TEST_TMP/legacy.out"
  local err_file="$TEST_TMP/legacy.err"
  local rc
  rc="$(run_launcher "$out_file" "$err_file" WRIX_PODMAN_SOCKET=1)"
  if [[ "$rc" != "0" ]]; then
    fail "legacy env launch exited $rc; stderr=$(tr '\n' ' ' < "$err_file")"
    return 1
  fi
  assert_absent "legacy env launch" "$PODMAN_LOG" "/run/podman/podman.sock" || return 1
  assert_absent "legacy env launch" "$PODMAN_LOG" "CONTAINER_HOST" || return 1
  assert_absent "legacy env launch" "$PODMAN_LOG" "GC_HOST_WORKSPACE" || return 1
  assert_absent "legacy env launch" "$PODMAN_LOG" "GC_HOST_BEADS" || return 1
}

test_unsafe_podman_socket_opt_in_mounts_real_socket() {
  local out_file="$TEST_TMP/unsafe-present.out"
  local err_file="$TEST_TMP/unsafe-present.err"
  local socket_dir="$RUNTIME_DIR/podman"
  local socket_path="$socket_dir/podman.sock"
  local listener_pid
  local rc
  local _attempt
  mkdir -p "$socket_dir"
  python3 - "$socket_path" <<'PY' &
import socket
import sys
import time

listener = socket.socket(socket.AF_UNIX)
listener.bind(sys.argv[1])
time.sleep(30)
PY
  listener_pid=$!
  for _attempt in {1..50}; do
    [[ -S "$socket_path" ]] && break
    sleep 0.02
  done
  rc="$(run_launcher "$out_file" "$err_file" WRIX_UNSAFE_PODMAN_SOCKET=1)"
  kill "$listener_pid" 2>/dev/null || true # best-effort: listener may have exited after a launcher failure.
  wait "$listener_pid" 2>/dev/null || true # best-effort: listener termination is expected.
  rm -f "$socket_path"
  if [[ "$rc" != "0" ]]; then
    fail "unsafe opt-in with socket exited $rc; stderr=$(tr '\n' ' ' < "$err_file")"
    return 1
  fi
  assert_contains "unsafe socket mount" "$PODMAN_LOG" "-v $socket_path:/run/podman/podman.sock" || return 1
  assert_contains "unsafe socket env" "$PODMAN_LOG" "-e CONTAINER_HOST=unix:///run/podman/podman.sock" || return 1
}

test_unsafe_podman_socket_opt_in_fails_loud_without_socket() {
  local out_file="$TEST_TMP/unsafe-missing.out"
  local err_file="$TEST_TMP/unsafe-missing.err"
  local rc
  rc="$(run_launcher "$out_file" "$err_file" WRIX_UNSAFE_PODMAN_SOCKET=1)"
  if [[ "$rc" = "0" ]]; then
    fail "unsafe opt-in without socket unexpectedly succeeded; stdout=$(tr '\n' ' ' < "$out_file")"
    return 1
  fi
  assert_contains "unsafe missing socket" "$err_file" "WRIX_UNSAFE_PODMAN_SOCKET set but socket not found" || return 1
}

ALL_TESTS=(
  test_default_launch_omits_host_podman_socket
  test_legacy_podman_socket_env_is_ignored
  test_unsafe_podman_socket_opt_in_mounts_real_socket
  test_unsafe_podman_socket_opt_in_fails_loud_without_socket
)

run_all() {
  local failed=0
  local test_name
  for test_name in "${ALL_TESTS[@]}"; do
    echo "=== $test_name ==="
    if "$test_name"; then
      printf '  PASS: %s\n' "$test_name"
    else
      failed=$((failed + 1))
    fi
  done
  [[ "$failed" -eq 0 ]]
}

if [[ $# -eq 0 ]]; then
  run_all
else
  if ! declare -f "$1" >/dev/null 2>&1; then
    echo "Unknown function: $1" >&2
    exit 1
  fi
  "$1"
fi
