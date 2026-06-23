#!/usr/bin/env bash
set -euo pipefail

TCP_PORT=5959
DARWIN_GATEWAY="192.168.64.1"
LINUX_SOCKET="/run/wrix/notify.sock"
CONNECT_TIMEOUT_SECONDS=3
TEST_TMP=""
BACKGROUND_PIDS=()

ensure_tmp() {
  if [[ -z "$TEST_TMP" ]]; then
    TEST_TMP=$(mktemp -d -t wrix-notify-test.XXXXXX)
    trap cleanup EXIT
  fi
}

cleanup() {
  local pid

  for pid in "${BACKGROUND_PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true # best-effort: test listeners may already have exited.
    wait "$pid" 2>/dev/null || true # best-effort: reap listeners that are still tracked.
  done
  if [[ -n "$TEST_TMP" ]]; then
    rm -rf "$TEST_TMP"
  fi
}

fail() {
  local message="$1"

  echo "FAIL: $message" >&2
  exit 1
}

fail_with_output() {
  local message="$1"
  local output_file="$2"

  echo "FAIL: $message" >&2
  if [[ -s "$output_file" ]]; then
    sed 's/^/  /' "$output_file" >&2
  fi
  exit 1
}

skip() {
  local message="$1"

  echo "SKIP: $message"
  exit 77
}

pass() {
  local message="$1"

  echo "PASS: $message"
}

require_command() {
  local name="$1"

  if ! command -v "$name" >/dev/null 2>&1; then
    fail "required command not found on PATH: $name"
  fi
}

require_command_or_skip() {
  local name="$1"

  if ! command -v "$name" >/dev/null 2>&1; then
    skip "command not available on this platform: $name"
  fi
}

resolve_repo_root() {
  local git_root

  if [[ -n "${REPO_ROOT:-}" ]]; then
    printf '%s\n' "$REPO_ROOT"
    return 0
  fi

  if git_root=$(git rev-parse --show-toplevel 2>/dev/null); then
    printf '%s\n' "$git_root"
    return 0
  fi

  pwd
}

wait_for_unix_socket() {
  local socket="$1"
  local attempt

  for ((attempt = 0; attempt < 50; attempt += 1)); do
    if [[ -S "$socket" ]]; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

wait_for_tcp_listener() {
  local host="$1"
  local port="$2"
  local attempt

  for ((attempt = 0; attempt < 50; attempt += 1)); do
    if nc -z -w "$CONNECT_TIMEOUT_SECONDS" "$host" "$port" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

wait_for_capture() {
  local capture="$1"
  local attempt

  for ((attempt = 0; attempt < 50; attempt += 1)); do
    if [[ -s "$capture" ]]; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

start_unix_capture() {
  local socket="$1"
  local capture="$2"
  local log_file="$3"
  local pid

  mkdir -p "$(dirname "$socket")"
  rm -f "$socket"
  : >"$capture"
  socat -u UNIX-LISTEN:"$socket",fork OPEN:"$capture",creat,append >"$log_file" 2>&1 &
  pid=$!
  BACKGROUND_PIDS+=("$pid")
  wait_for_unix_socket "$socket"
}

start_tcp_capture() {
  local host="$1"
  local port="$2"
  local capture="$3"
  local log_file="$4"
  local pid

  : >"$capture"
  socat -u TCP-LISTEN:"$port",bind="$host",fork,reuseaddr OPEN:"$capture",creat,append >"$log_file" 2>&1 &
  pid=$!
  BACKGROUND_PIDS+=("$pid")
  wait_for_tcp_listener "$host" "$port"
}

assert_json_field() {
  local capture="$1"
  local field="$2"
  local expected="$3"
  local actual

  actual=$(tail -n 1 "$capture" | jq -r --arg field "$field" '.[$field]')
  if [[ "$actual" != "$expected" ]]; then
    fail "captured .$field was '$actual', expected '$expected'"
  fi
}

run_notify_with_timeout() {
  local output_file="$1"
  local title="$2"
  local message="$3"
  local sound="$4"
  local rc=0

  timeout 1s wrix-notify "$title" "$message" "$sound" >"$output_file" 2>&1 || rc=$?
  if [[ "$rc" -eq 124 ]]; then
    fail_with_output "wrix-notify waited for an acknowledgement" "$output_file"
  fi
  if [[ "$rc" -ne 0 ]]; then
    fail_with_output "wrix-notify exited non-zero" "$output_file"
  fi
}

test_client_tcp_endpoint_override() {
  ensure_tmp
  require_command jq
  require_command nc
  require_command socat
  require_command timeout
  require_command wrix-notify

  local capture="$TEST_TMP/tcp-capture.jsonl"
  local listener_log="$TEST_TMP/tcp-listener.log"
  local output_file="$TEST_TMP/wrix-notify.log"
  local port=$((42000 + (BASHPID % 20000)))
  local title="notify tcp override $BASHPID"
  local message="client payload reached fake daemon"
  local sound="Ping"
  local session_id="notify-test:0.1"

  if ! start_tcp_capture "127.0.0.1" "$port" "$capture" "$listener_log"; then
    fail_with_output "could not start TCP capture listener" "$listener_log"
  fi

  WRIX_NOTIFY_TCP="127.0.0.1:$port" \
    WRIX_SESSION_ID="$session_id" \
    run_notify_with_timeout "$output_file" "$title" "$message" "$sound"

  if ! wait_for_capture "$capture"; then
    fail_with_output "fake TCP daemon did not receive wrix-notify payload" "$listener_log"
  fi

  assert_json_field "$capture" title "$title"
  assert_json_field "$capture" message "$message"
  assert_json_field "$capture" sound "$sound"
  assert_json_field "$capture" session_id "$session_id"
  pass "wrix-notify honors WRIX_NOTIFY_TCP=host:port and sends one JSON envelope"
}

write_spawn_config() {
  local output_file="$1"
  local workspace="$2"
  local title="$3"
  local message="$4"
  local sound="$5"
  local session_id="$6"

  jq -n \
    --arg workspace "$workspace" \
    --arg title "$title" \
    --arg message "$message" \
    --arg sound "$sound" \
    --arg session_id "$session_id" \
    '{
      workspace: $workspace,
      env: [
        ["WRIX_NOTIFY_TEST_IN_CONTAINER", "1"],
        ["WRIX_NOTIFY_TEST_TITLE", $title],
        ["WRIX_NOTIFY_TEST_MESSAGE", $message],
        ["WRIX_NOTIFY_TEST_SOUND", $sound],
        ["WRIX_SESSION_ID", $session_id]
      ],
      agent_args: ["bash", "/workspace/notify-test.sh", "--inside-container"]
    }' >"$output_file"
}

run_spawned_container_check() {
  local spawn_config="$1"
  local output_file="$2"
  local repo_root="$3"
  local runtime_dir="$4"
  local deploy_key="$5"
  local rc=0

  (
    cd "$repo_root"
    XDG_RUNTIME_DIR="$runtime_dir" \
      WRIX_DEPLOY_KEY="$deploy_key" \
      WRIX_GIT_SIGN=0 \
      nix run --no-warn-dirty .#sandbox-base -- spawn --spawn-config "$spawn_config"
  ) >"$output_file" 2>&1 || rc=$?

  if [[ "$rc" -ne 0 ]]; then
    fail_with_output "wrix spawn notification check failed" "$output_file"
  fi
}

test_container_payload_inside() {
  require_command timeout
  require_command wrix-notify

  local title="${WRIX_NOTIFY_TEST_TITLE:?}"
  local message="${WRIX_NOTIFY_TEST_MESSAGE:?}"
  local sound="${WRIX_NOTIFY_TEST_SOUND:?}"
  local output_file="/tmp/wrix-notify-inside.log"

  if [[ -n "${WRIX_NOTIFY_TCP:-}" ]]; then
    if [[ "$WRIX_NOTIFY_TCP" != *:* ]]; then
      fail "WRIX_NOTIFY_TCP inside the container is not host:port: $WRIX_NOTIFY_TCP"
    fi
  elif [[ ! -S "$LINUX_SOCKET" ]]; then
    fail "notification socket was not mounted at $LINUX_SOCKET"
  fi

  run_notify_with_timeout "$output_file" "$title" "$message" "$sound"
}

test_container_transport_linux() {
  ensure_tmp
  require_command jq
  require_command nc
  require_command nix
  require_command socat
  require_command_or_skip podman

  if ! podman info >/dev/null 2>&1; then
    skip "podman runtime is not available"
  fi

  local runtime_dir="$TEST_TMP/runtime"
  local socket_dir="$runtime_dir/wrix"
  local socket="$socket_dir/notify.sock"
  local capture="$TEST_TMP/linux-capture.jsonl"
  local listener_log="$TEST_TMP/linux-listener.log"
  local output_file="$TEST_TMP/wrix-spawn.log"
  local workspace="$TEST_TMP/workspace"
  local spawn_config="$TEST_TMP/spawn.json"
  local deploy_key="$TEST_TMP/deploy_key"
  local title="notify container linux $BASHPID"
  local message="container payload reached unix daemon"
  local sound="Ping"
  local session_id="notify-test:0.1"
  local repo_root

  repo_root=$(resolve_repo_root)
  mkdir -p "$workspace" "$socket_dir"
  cp "$repo_root/tests/standalone/notify-test.sh" "$workspace/notify-test.sh"
  printf 'not-a-real-key\n' >"$deploy_key"
  chmod 600 "$deploy_key"

  if ! start_unix_capture "$socket" "$capture" "$listener_log"; then
    fail_with_output "could not start Unix socket capture listener" "$listener_log"
  fi

  write_spawn_config "$spawn_config" "$workspace" "$title" "$message" "$sound" "$session_id"
  run_spawned_container_check "$spawn_config" "$output_file" "$repo_root" "$runtime_dir" "$deploy_key"

  if ! wait_for_capture "$capture"; then
    fail_with_output "fake Unix daemon did not receive container wrix-notify payload" "$listener_log"
  fi

  assert_json_field "$capture" title "$title"
  assert_json_field "$capture" message "$message"
  assert_json_field "$capture" sound "$sound"
  assert_json_field "$capture" session_id "$session_id"
  pass "container wrix-notify reaches host Unix socket daemon"
}

test_container_transport_darwin() {
  ensure_tmp
  require_command jq
  require_command nc
  require_command nix
  require_command socat
  require_command_or_skip container

  local capture="$TEST_TMP/darwin-capture.jsonl"
  local listener_log="$TEST_TMP/darwin-listener.log"
  local output_file="$TEST_TMP/wrix-spawn.log"
  local workspace="$TEST_TMP/workspace"
  local spawn_config="$TEST_TMP/spawn.json"
  local deploy_key="$TEST_TMP/deploy_key"
  local title="notify container darwin $BASHPID"
  local message="container payload reached tcp daemon"
  local sound="Ping"
  local session_id="notify-test:0.1"
  local repo_root

  if ! start_tcp_capture "$DARWIN_GATEWAY" "$TCP_PORT" "$capture" "$listener_log"; then
    skip "could not bind fake daemon to $DARWIN_GATEWAY:$TCP_PORT"
  fi

  repo_root=$(resolve_repo_root)
  mkdir -p "$workspace"
  cp "$repo_root/tests/standalone/notify-test.sh" "$workspace/notify-test.sh"
  printf 'not-a-real-key\n' >"$deploy_key"
  chmod 600 "$deploy_key"

  write_spawn_config "$spawn_config" "$workspace" "$title" "$message" "$sound" "$session_id"
  run_spawned_container_check "$spawn_config" "$output_file" "$repo_root" "$TEST_TMP/runtime" "$deploy_key"

  if ! wait_for_capture "$capture"; then
    fail_with_output "fake TCP daemon did not receive container wrix-notify payload" "$listener_log"
  fi

  assert_json_field "$capture" title "$title"
  assert_json_field "$capture" message "$message"
  assert_json_field "$capture" sound "$sound"
  assert_json_field "$capture" session_id "$session_id"
  pass "container wrix-notify reaches host TCP daemon"
}

test_container_transport() {
  if [[ "${WRIX_NOTIFY_TEST_IN_CONTAINER:-}" == "1" ]]; then
    test_container_payload_inside
    return 0
  fi

  case "$(uname -s)" in
    Linux) test_container_transport_linux ;;
    Darwin) test_container_transport_darwin ;;
    *) skip "unsupported platform: $(uname -s)" ;;
  esac
}

test_macos_tcp_bind_address() {
  local repo_root
  local daemon_file

  repo_root=$(resolve_repo_root)
  daemon_file="$repo_root/lib/notify/daemon.nix"

  if ! grep -Eq 'TCP-LISTEN:\$\{tcpPort\},bind=192\.168\.64\.1' "$daemon_file"; then
    fail "wrix-notifyd TCP listener does not bind to 192.168.64.1"
  fi
  if grep -Eq 'TCP-LISTEN:.*bind=0\.0\.0\.0' "$daemon_file"; then
    fail "wrix-notifyd TCP listener binds to 0.0.0.0"
  fi
  pass "wrix-notifyd binds the TCP listener to 192.168.64.1 only"
}

test_claude_stop_hook_config() {
  local repo_root
  local sandbox_file

  repo_root=$(resolve_repo_root)
  sandbox_file="$repo_root/lib/sandbox/default.nix"

  if ! grep -Eq 'Stop = \[' "$sandbox_file"; then
    fail "Claude settings do not define a Stop hook"
  fi
  if grep -Eq 'Notification = \[' "$sandbox_file"; then
    fail "Claude settings still define the notify command under Notification"
  fi
  if ! grep -Eq 'command = "wrix-notify' "$sandbox_file"; then
    fail "Claude Stop hook does not invoke wrix-notify"
  fi
  pass "Claude settings invoke wrix-notify from a Stop hook"
}

main() {
  local test_name="${1:-test_container_transport}"

  case "$test_name" in
    --inside-container) test_container_payload_inside ;;
    test_client_tcp_endpoint_override) test_client_tcp_endpoint_override ;;
    test_claude_stop_hook_config) test_claude_stop_hook_config ;;
    test_container_transport) test_container_transport ;;
    test_macos_tcp_bind_address) test_macos_tcp_bind_address ;;
    *) fail "unknown notify test: $test_name" ;;
  esac
}

main "$@"
