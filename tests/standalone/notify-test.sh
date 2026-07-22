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

wait_for_capture_within_one_second() {
  local capture="$1"
  local attempt

  for ((attempt = 0; attempt < 100; attempt += 1)); do
    if [[ -s "$capture" ]]; then
      return 0
    fi
    sleep 0.01
  done
  [[ -s "$capture" ]]
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

start_notify_daemon() {
  local runtime_dir="$1"
  local capture="$2"
  local log_file="$3"
  local pid

  mkdir -p "$runtime_dir/wrix"
  : >"$capture"
  XDG_RUNTIME_DIR="$runtime_dir" \
    WRIX_NOTIFY_ALWAYS=1 \
    WRIX_NOTIFY_TEST_DISPATCH_CAPTURE="$capture" \
    wrix-notifyd >"$log_file" 2>&1 &
  pid=$!
  BACKGROUND_PIDS+=("$pid")

  case "$(uname -s)" in
    Linux) wait_for_unix_socket "$runtime_dir/wrix/notify.sock" ;;
    Darwin) wait_for_tcp_listener "$DARWIN_GATEWAY" "$TCP_PORT" ;;
    *) return 1 ;;
  esac
}

assert_single_json_envelope() {
  local capture="$1"
  local count

  if ! count=$(jq -s 'length' "$capture"); then
    fail "captured payload was not valid JSONL"
  fi
  if [[ "$count" != "1" ]]; then
    fail "captured $count JSON envelopes, expected 1"
  fi
}

assert_json_field() {
  local capture="$1"
  local field="$2"
  local expected="$3"
  local actual

  actual=$(jq -sr --arg field "$field" '.[0][$field]' "$capture")
  if [[ "$actual" != "$expected" ]]; then
    fail "captured .$field was '$actual', expected '$expected'"
  fi
}

assert_native_dispatch() {
  local capture="$1"
  local title="$2"
  local message="$3"
  local sound="$4"

  case "$(uname -s)" in
    Linux)
      if ! jq -se --arg title "$title" --arg message "$message" \
        'length == 1 and .[0] == [$title, $message]' "$capture" >/dev/null; then
        fail_with_output "wrix-notifyd did not dispatch the Linux notification payload" "$capture"
      fi
      ;;
    Darwin)
      if ! jq -se --arg title "$title" --arg message "$message" --arg sound "$sound" \
        'length == 1 and .[0] == ["-title", $title, "-message", $message, "-sound", $sound]' \
        "$capture" >/dev/null; then
        fail_with_output "wrix-notifyd did not dispatch the Darwin notification payload" "$capture"
      fi
      ;;
    *) fail "unsupported platform: $(uname -s)" ;;
  esac
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

run_notify_and_assert_capture_latency() {
  local output_file="$1"
  local capture="$2"
  local title="$3"
  local message="$4"
  local sound="$5"
  local notify_pid
  local rc=0

  timeout 1s wrix-notify "$title" "$message" "$sound" >"$output_file" 2>&1 &
  notify_pid=$!

  if ! wait_for_capture_within_one_second "$capture"; then
    kill "$notify_pid" 2>/dev/null || true # best-effort: timed-out client may already have exited.
    wait "$notify_pid" 2>/dev/null || true # best-effort: reap the timed-out client after failure.
    fail_with_output "wrix-notify payload was not captured within one second" "$output_file"
  fi

  wait "$notify_pid" || rc=$?
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
    run_notify_and_assert_capture_latency "$output_file" "$capture" "$title" "$message" "$sound"

  assert_single_json_envelope "$capture"
  assert_json_field "$capture" title "$title"
  assert_json_field "$capture" message "$message"
  assert_json_field "$capture" sound "$sound"
  assert_json_field "$capture" session_id "$session_id"
  pass "wrix-notify honors WRIX_NOTIFY_TCP=host:port and sends one JSON envelope within one second"
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
      nix run --no-warn-dirty .#sandbox -- spawn --spawn-config "$spawn_config"
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
  require_command wrix-notifyd
  require_command_or_skip podman

  if ! podman info >/dev/null 2>&1; then
    skip "podman runtime is not available"
  fi
  if [[ ! -c /dev/net/tun ]]; then
    skip "podman runtime cannot launch wrix networking without /dev/net/tun"
  fi

  local runtime_dir="$TEST_TMP/runtime"
  local capture="$TEST_TMP/linux-dispatch.jsonl"
  local daemon_log="$TEST_TMP/wrix-notifyd.log"
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
  mkdir -p "$runtime_dir/libpod/tmp" "$workspace"
  cp "$repo_root/tests/standalone/notify-test.sh" "$workspace/notify-test.sh"
  printf 'not-a-real-key\n' >"$deploy_key"
  chmod 600 "$deploy_key"

  if ! start_notify_daemon "$runtime_dir" "$capture" "$daemon_log"; then
    fail_with_output "could not start wrix-notifyd Unix socket listener" "$daemon_log"
  fi

  write_spawn_config "$spawn_config" "$workspace" "$title" "$message" "$sound" "$session_id"
  run_spawned_container_check "$spawn_config" "$output_file" "$repo_root" "$runtime_dir" "$deploy_key"

  if ! wait_for_capture "$capture"; then
    fail_with_output "wrix-notifyd did not dispatch the container payload" "$daemon_log"
  fi

  assert_native_dispatch "$capture" "$title" "$message" "$sound"
  pass "container wrix-notify reaches the host Unix socket daemon and native bridge"
}

test_container_transport_darwin() {
  ensure_tmp
  require_command jq
  require_command nc
  require_command nix
  require_command wrix-notifyd
  require_command_or_skip container

  local runtime_dir="$TEST_TMP/runtime"
  local capture="$TEST_TMP/darwin-dispatch.jsonl"
  local daemon_log="$TEST_TMP/wrix-notifyd.log"
  local output_file="$TEST_TMP/wrix-spawn.log"
  local workspace="$TEST_TMP/workspace"
  local spawn_config="$TEST_TMP/spawn.json"
  local deploy_key="$TEST_TMP/deploy_key"
  local title="notify container darwin $BASHPID"
  local message="container payload reached tcp daemon"
  local sound="Ping"
  local session_id="notify-test:0.1"
  local repo_root

  repo_root=$(resolve_repo_root)
  mkdir -p "$workspace"
  cp "$repo_root/tests/standalone/notify-test.sh" "$workspace/notify-test.sh"
  printf 'not-a-real-key\n' >"$deploy_key"
  chmod 600 "$deploy_key"

  if ! start_notify_daemon "$runtime_dir" "$capture" "$daemon_log"; then
    fail_with_output "could not start wrix-notifyd TCP listener" "$daemon_log"
  fi

  write_spawn_config "$spawn_config" "$workspace" "$title" "$message" "$sound" "$session_id"
  run_spawned_container_check "$spawn_config" "$output_file" "$repo_root" "$runtime_dir" "$deploy_key"

  if ! wait_for_capture "$capture"; then
    fail_with_output "wrix-notifyd did not dispatch the container payload" "$daemon_log"
  fi

  assert_native_dispatch "$capture" "$title" "$message" "$sound"
  pass "container wrix-notify reaches the host TCP daemon and native bridge"
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
  require_command nix

  local repo_root
  local daemon_text

  repo_root=$(resolve_repo_root)
  if ! daemon_text=$(REPO_ROOT="$repo_root" nix eval --raw --impure --expr '
    let
      root = builtins.getEnv "REPO_ROOT";
      fakePkgs = {
        stdenv = { isDarwin = true; };
        bash = "bash";
        coreutils = "coreutils";
        jq = "jq";
        socat = "socat";
        terminal-notifier = "terminal-notifier";
        libnotify = "libnotify";
        writeShellApplication = args: args.text;
      };
    in
    import (root + "/lib/notify/daemon.nix") { pkgs = fakePkgs; }
  '); then
    fail "could not evaluate generated Darwin wrix-notifyd launcher text"
  fi

  if [[ "$daemon_text" != *"TCP-LISTEN:5959,bind=192.168.64.1"* ]]; then
    fail "generated Darwin wrix-notifyd listener does not bind to 192.168.64.1"
  fi
  if [[ "$daemon_text" == *"TCP-LISTEN:"*"bind=0.0.0.0"* ]]; then
    fail "generated Darwin wrix-notifyd listener binds to 0.0.0.0"
  fi
  pass "wrix-notifyd binds the TCP listener to 192.168.64.1 only"
}

test_claude_stop_hook_config() {
  require_command jq
  require_command nix

  local repo_root
  local settings_file
  local system

  repo_root=$(resolve_repo_root)
  system=$(nix eval --raw --impure --expr builtins.currentSystem)
  if ! settings_file=$(nix build --no-link --print-out-paths --no-warn-dirty \
    "$repo_root#packages.$system.sandbox-claude.passthru.image.claudeSettingsJson"); then
    fail "could not build generated Claude settings"
  fi

  if ! jq -e '((.hooks.Stop // []) | [ .[] | select(((.hooks // []) | any(.type == "command" and ((.command // "") | startswith("wrix-notify"))))) ] | length) > 0' "$settings_file" >/dev/null; then
    fail "generated Claude settings do not invoke wrix-notify from a Stop hook"
  fi
  if jq -e '(.hooks // {}) | has("Notification")' "$settings_file" >/dev/null; then
    fail "generated Claude settings still define the notify command under Notification"
  fi
  pass "Claude settings invoke wrix-notify from a generated Stop hook"
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
