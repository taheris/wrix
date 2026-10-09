#!/usr/bin/env bash
set -euo pipefail

TCP_PORT=5959
DARWIN_GATEWAY="192.168.64.1"
LINUX_SOCKET="/run/wrix/notify.sock"
CONNECT_TIMEOUT_SECONDS=3
TEST_TMP=""
BACKGROUND_PIDS=()
BACKGROUND_GROUPS=()

ensure_tmp() {
  if [[ -z "$TEST_TMP" ]]; then
    TEST_TMP=$(mktemp -d -t wrix-notify-test.XXXXXX)
    trap cleanup EXIT
  fi
}

cleanup() {
  local pid

  for pid in "${BACKGROUND_PIDS[@]}"; do
    kill_background_process "$pid" 2>/dev/null || true # best-effort: test listeners may already have exited.
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

wait_for_text() {
  local file="$1"
  local expected="$2"
  local attempt

  for ((attempt = 0; attempt < 50; attempt += 1)); do
    if [[ -f "$file" && "$(<"$file")" == *"$expected"* ]]; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

start_tcp_capture() {
  local host="$1"
  local port="$2"
  local capture="$3"
  local log_file="$4"
  local pid

  : >"$capture"
  socat -u TCP-LISTEN:"$port",bind="$host",fork,reuseaddr OPEN:"$capture",creat,append >"$log_file" 2>&1 &
  pid="$!"
  BACKGROUND_PIDS+=("$pid")
  wait_for_tcp_listener "$host" "$port"
}

start_non_acknowledging_tcp_capture() {
  local host="$1"
  local port="$2"
  local capture="$3"
  local log_file="$4"
  local handler="$TEST_TMP/non-acknowledging-handler.sh"
  local pid
  local probe_rc=0

  : >"$capture"
  cat >"$handler" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

: "${WRIX_NOTIFY_TEST_HOLD_CAPTURE:?}"
line=""
if IFS= read -r line && [[ -n "$line" ]]; then
  printf '%s\n' "$line" >>"$WRIX_NOTIFY_TEST_HOLD_CAPTURE"
fi
sleep 2
EOF
  chmod +x "$handler"
  WRIX_NOTIFY_TEST_HOLD_CAPTURE="$capture" \
    socat TCP-LISTEN:"$port",bind="$host",fork,reuseaddr EXEC:"$handler" >"$log_file" 2>&1 &
  pid="$!"
  BACKGROUND_PIDS+=("$pid")
  wait_for_tcp_listener "$host" "$port"

  # shellcheck disable=SC2016 # $1 and $2 intentionally expand in the nested Bash process.
  timeout 1s bash -c \
    'exec 3<>"/dev/tcp/$1/$2"; printf "%s\n" "fixture conformance probe" >&3; IFS= read -r _ <&3' \
    _ "$host" "$port" >/dev/null 2>&1 || probe_rc="$?"
  if [[ "$probe_rc" -ne 124 ]]; then
    printf 'listener conformance probe exited %s instead of blocking\n' "$probe_rc" >>"$log_file"
    return 1
  fi
  : >"$capture"
}

start_notify_daemon() {
  require_command python3
  local runtime_dir="$1"
  local capture="$2"
  local log_file="$3"
  local always="$4"
  local verbose="$5"
  local dispatch_time_capture="$6"
  local pid

  mkdir -p "$runtime_dir/wrix" "$runtime_dir/data"
  : >"$capture"
  XDG_DATA_HOME="$runtime_dir/data" \
    XDG_RUNTIME_DIR="$runtime_dir" \
    WRIX_NOTIFY_ALWAYS="$always" \
    WRIX_NOTIFY_VERBOSE="$verbose" \
    WRIX_NOTIFY_TEST_DISPATCH_CAPTURE="$capture" \
    WRIX_NOTIFY_TEST_DISPATCH_TIME_CAPTURE="$dispatch_time_capture" \
    python3 -c 'import os; os.setsid(); os.execvp("wrix-notifyd", ["wrix-notifyd"])' >"$log_file" 2>&1 &
  pid="$!"
  BACKGROUND_PIDS+=("$pid")
  BACKGROUND_GROUPS+=("$pid")

  case "$(uname -s)" in
    Linux) wait_for_unix_socket "$runtime_dir/wrix/notify.sock" ;;
    Darwin) wait_for_tcp_listener "$DARWIN_GATEWAY" "$TCP_PORT" ;;
    *) return 1 ;;
  esac
}

kill_background_process() {
  local pid="$1"
  local group
  for group in "${BACKGROUND_GROUPS[@]}"; do
    if [[ "$group" == "$pid" ]]; then
      kill -- "-$pid"
      return
    fi
  done
  kill "$pid"
}

stop_background_process() {
  local pid="$1"
  local tracked
  local -a remaining=()

  kill_background_process "$pid"
  wait "$pid" 2>/dev/null || true # best-effort: a terminated daemon exits non-zero when reaped.
  for tracked in "${BACKGROUND_PIDS[@]}"; do
    if [[ "$tracked" != "$pid" ]]; then remaining+=("$tracked"); fi
  done
  BACKGROUND_PIDS=("${remaining[@]}")
}

send_daemon_envelope() {
  local runtime_dir="$1"
  local payload="$2"

  case "$(uname -s)" in
    Linux) printf '%s\n' "$payload" | socat -u - "UNIX-CONNECT:$runtime_dir/wrix/notify.sock" ;;
    Darwin) printf '%s\n' "$payload" | socat -u - "TCP:$DARWIN_GATEWAY:$TCP_PORT" ;;
    *) fail "unsupported platform: $(uname -s)" ;;
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
        'length == 1 and .[0] == (["-title", $title, "-message", $message] +
          if $sound == "" then [] else ["-sound", $sound] end)' \
        "$capture" >/dev/null; then
        fail_with_output "wrix-notifyd did not dispatch the Darwin notification payload" "$capture"
      fi
      ;;
    *) fail "unsupported platform: $(uname -s)" ;;
  esac
}

assert_dispatch_latency() {
  local start_time_file="$1"
  local dispatch_time_file="$2"
  local start_time
  local dispatch_time
  local elapsed

  if [[ ! -s "$start_time_file" || ! -s "$dispatch_time_file" ]]; then
    fail "notification latency timestamps were not recorded"
  fi
  start_time=$(<"$start_time_file")
  dispatch_time=$(<"$dispatch_time_file")
  if [[ ! "$start_time" =~ ^[0-9]+$ || ! "$dispatch_time" =~ ^[0-9]+$ ]]; then
    fail "notification latency timestamps were not numeric"
  fi
  elapsed="$((dispatch_time - start_time))"
  if [[ "$elapsed" -lt 0 || "$elapsed" -ge 1000000000 ]]; then
    fail "native bridge dispatch took $elapsed nanoseconds, expected less than one second"
  fi
}

run_notify_with_timeout() {
  local output_file="$1"
  local title="$2"
  local message="$3"
  local sound="$4"
  local rc=0

  timeout 1s wrix-notify "$title" "$message" "$sound" >"$output_file" 2>&1 || rc="$?"
  if [[ "$rc" -eq 124 ]]; then
    fail_with_output "wrix-notify waited for an acknowledgement" "$output_file"
  fi
  if [[ "$rc" -ne 0 ]]; then
    fail_with_output "wrix-notify exited non-zero" "$output_file"
  fi
}

test_client_tcp_endpoint_override() {
  ensure_tmp
  require_command nc
  require_command socat
  require_command wrix-notify

  local capture="$TEST_TMP/tcp-endpoint-capture.jsonl"
  local listener_log="$TEST_TMP/tcp-endpoint-listener.log"
  local port="$((42000 + (BASHPID % 20000)))"

  if ! start_tcp_capture "127.0.0.1" "$port" "$capture" "$listener_log"; then
    fail_with_output "could not start TCP capture listener" "$listener_log"
  fi

  WRIX_NOTIFY_TCP="127.0.0.1:$port" wrix-notify "endpoint override" "selected listener"
  if ! wait_for_capture "$capture"; then
    fail_with_output "WRIX_NOTIFY_TCP payload did not reach the selected endpoint" "$listener_log"
  fi
  pass "WRIX_NOTIFY_TCP selects the client TCP endpoint"
}

test_focus_target_envelope() {
  ensure_tmp
  require_command jq
  require_command nc
  require_command socat
  require_command wrix-notify

  local capture="$TEST_TMP/client-envelope.jsonl"
  local listener_log="$TEST_TMP/client-envelope-listener.log"
  local port="$((42000 + (BASHPID % 20000)))"
  local title="notify envelope $BASHPID"
  local message="client payload fields"
  local sound="Ping"
  local target
  local case_name
  local -a focus_env

  if ! start_tcp_capture "127.0.0.1" "$port" "$capture" "$listener_log"; then
    fail_with_output "could not start TCP capture listener" "$listener_log"
  fi

  for case_name in opaque whitespace empty absent; do
    case "$case_name" in
      opaque) target=$' host/é:"quoted"\n'; focus_env=("WRIX_FOCUS_TARGET=$target") ;;
      whitespace) target=" "; focus_env=("WRIX_FOCUS_TARGET=$target") ;;
      empty) target=""; focus_env=("WRIX_FOCUS_TARGET=") ;;
      absent) target=""; focus_env=(-u WRIX_FOCUS_TARGET) ;;
    esac
    : >"$capture"
    env "${focus_env[@]}" WRIX_NOTIFY_TCP="127.0.0.1:$port" \
      WRIX_SESSION_ID="legacy:0.1" WRIX_EXECUTION_ID="execution:0.1" \
      PI_SESSION_ID="conversation:0.1" TMUX="debugging-container" \
      wrix-notify "$title" "$message" "$sound"
    if ! wait_for_capture "$capture"; then
      fail_with_output "wrix-notify $case_name envelope was not captured" "$listener_log"
    fi
    assert_single_json_envelope "$capture"
    assert_json_field "$capture" title "$title"
    assert_json_field "$capture" message "$message"
    assert_json_field "$capture" sound "$sound"
    if ! jq -se --arg target "$target" '
      .[0] | (has("session_id") | not) and
      (if $target == "" then (has("focus_target") | not) else .focus_target == $target end)
    ' "$capture" >/dev/null; then
      fail_with_output "focus input was aliased, changed, or not omitted for $case_name" "$capture"
    fi
  done
  pass "packaged client copies opaque focus exactly and omits absent/empty focus without identity aliases"
}

test_client_non_blocking() {
  ensure_tmp
  require_command nc
  require_command socat
  require_command timeout
  require_command wrix-notify

  local capture="$TEST_TMP/non-blocking-capture.jsonl"
  local listener_log="$TEST_TMP/non-blocking-listener.log"
  local output_file="$TEST_TMP/non-blocking-client.log"
  local port="$((42000 + (BASHPID % 20000)))"

  if ! start_non_acknowledging_tcp_capture "127.0.0.1" "$port" "$capture" "$listener_log"; then
    fail_with_output "could not start non-acknowledging TCP listener" "$listener_log"
  fi

  WRIX_NOTIFY_TCP="127.0.0.1:$port" \
    run_notify_with_timeout "$output_file" "non-blocking client" "no acknowledgement" "Ping"
  if ! wait_for_capture "$capture"; then
    fail_with_output "non-acknowledging listener did not capture the client payload" "$listener_log"
  fi
  pass "wrix-notify exits while the server holds the unacknowledged connection open"
}

write_spawn_config() {
  local output_file="$1"
  local workspace="$2"
  local title="$3"
  local message="$4"
  local sound="$5"
  local focus_target="$6"

  jq -n \
    --arg workspace "$workspace" \
    --arg title "$title" \
    --arg message "$message" \
    --arg sound "$sound" \
    --arg focus_target "$focus_target" \
    '{
      workspace: $workspace,
      env: [
        ["WRIX_NOTIFY_TEST_IN_CONTAINER", "1"],
        ["WRIX_NOTIFY_TEST_TITLE", $title],
        ["WRIX_NOTIFY_TEST_MESSAGE", $message],
        ["WRIX_NOTIFY_TEST_SOUND", $sound],
        ["WRIX_NOTIFY_TEST_START_TIME_FILE", "/workspace/notify-started-ns"],
        ["WRIX_FOCUS_TARGET", $focus_target]
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
  ) >"$output_file" 2>&1 || rc="$?"

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
  local start_time_file="${WRIX_NOTIFY_TEST_START_TIME_FILE:-}"

  if [[ -n "${WRIX_NOTIFY_TCP:-}" ]]; then
    if [[ "$WRIX_NOTIFY_TCP" != *:* ]]; then
      fail "WRIX_NOTIFY_TCP inside the container is not host:port: $WRIX_NOTIFY_TCP"
    fi
  elif [[ ! -S "$LINUX_SOCKET" ]]; then
    fail "notification socket was not mounted at $LINUX_SOCKET"
  fi

  if [[ -n "$start_time_file" ]]; then
    date +%s%N >"$start_time_file"
  fi
  run_notify_with_timeout "$output_file" "$title" "$message" "$sound"
}

run_container_check_linux() {
  local assertion="$1"

  ensure_tmp
  require_command jq
  require_command nc
  require_command nix
  require_command wrix-notifyd
  require_command_or_skip podman

  if [[ "$(uname -s)" != "Linux" ]]; then
    skip "Linux notification transport is not available on this platform"
  fi
  if ! podman info >/dev/null 2>&1; then
    skip "podman runtime is not available"
  fi
  if [[ ! -c /dev/net/tun ]]; then
    skip "podman runtime cannot launch wrix networking without /dev/net/tun"
  fi

  local runtime_dir="$TEST_TMP/runtime"
  local capture="$TEST_TMP/linux-dispatch.jsonl"
  local dispatch_time="$TEST_TMP/linux-dispatch-ns"
  local daemon_log="$TEST_TMP/wrix-notifyd.log"
  local output_file="$TEST_TMP/wrix-spawn.log"
  local workspace="$TEST_TMP/workspace"
  local start_time="$workspace/notify-started-ns"
  local spawn_config="$TEST_TMP/spawn.json"
  local deploy_key="$TEST_TMP/deploy_key"
  local title="notify container linux $BASHPID"
  local message="container payload reached unix daemon"
  local sound="Ping"
  local focus_target="notify-test:0.1"
  local repo_root

  repo_root=$(resolve_repo_root)
  mkdir -p "$runtime_dir/libpod/tmp" "$workspace"
  cp "$repo_root/tests/standalone/notify-test.sh" "$workspace/notify-test.sh"
  printf 'not-a-real-key\n' >"$deploy_key"
  chmod 600 "$deploy_key"

  if ! start_notify_daemon "$runtime_dir" "$capture" "$daemon_log" "1" "0" "$dispatch_time"; then
    fail_with_output "could not start wrix-notifyd Unix socket listener" "$daemon_log"
  fi

  write_spawn_config "$spawn_config" "$workspace" "$title" "$message" "$sound" "$focus_target"
  run_spawned_container_check "$spawn_config" "$output_file" "$repo_root" "$runtime_dir" "$deploy_key"

  if ! wait_for_capture "$capture"; then
    fail_with_output "wrix-notifyd did not dispatch the container payload" "$daemon_log"
  fi

  case "$assertion" in
    latency)
      assert_dispatch_latency "$start_time" "$dispatch_time"
      pass "container notification reaches the Linux native bridge within one second"
      ;;
    transport)
      assert_native_dispatch "$capture" "$title" "$message" "$sound"
      pass "container wrix-notify reaches the host Unix socket daemon and native bridge"
      ;;
    *) fail "unknown Linux container assertion: $assertion" ;;
  esac
}

run_container_check_darwin() {
  local assertion="$1"

  ensure_tmp
  require_command jq
  require_command nc
  require_command nix
  require_command wrix-notifyd
  require_command_or_skip container

  if [[ "$(uname -s)" != "Darwin" ]]; then
    skip "Darwin notification transport is not available on this platform"
  fi

  local runtime_dir="$TEST_TMP/runtime"
  local capture="$TEST_TMP/darwin-dispatch.jsonl"
  local dispatch_time="$TEST_TMP/darwin-dispatch-ns"
  local daemon_log="$TEST_TMP/wrix-notifyd.log"
  local output_file="$TEST_TMP/wrix-spawn.log"
  local workspace="$TEST_TMP/workspace"
  local start_time="$workspace/notify-started-ns"
  local spawn_config="$TEST_TMP/spawn.json"
  local deploy_key="$TEST_TMP/deploy_key"
  local title="notify container darwin $BASHPID"
  local message="container payload reached tcp daemon"
  local sound="Ping"
  local focus_target="notify-test:0.1"
  local repo_root

  repo_root=$(resolve_repo_root)
  mkdir -p "$workspace"
  cp "$repo_root/tests/standalone/notify-test.sh" "$workspace/notify-test.sh"
  printf 'not-a-real-key\n' >"$deploy_key"
  chmod 600 "$deploy_key"

  if ! start_notify_daemon "$runtime_dir" "$capture" "$daemon_log" "1" "0" "$dispatch_time"; then
    fail_with_output "could not start wrix-notifyd TCP listener" "$daemon_log"
  fi

  write_spawn_config "$spawn_config" "$workspace" "$title" "$message" "$sound" "$focus_target"
  run_spawned_container_check "$spawn_config" "$output_file" "$repo_root" "$runtime_dir" "$deploy_key"

  if ! wait_for_capture "$capture"; then
    fail_with_output "wrix-notifyd did not dispatch the container payload" "$daemon_log"
  fi

  case "$assertion" in
    latency)
      assert_dispatch_latency "$start_time" "$dispatch_time"
      pass "container notification reaches the Darwin native bridge within one second"
      ;;
    transport)
      assert_native_dispatch "$capture" "$title" "$message" "$sound"
      pass "container wrix-notify reaches the host TCP daemon and native bridge"
      ;;
    *) fail "unknown Darwin container assertion: $assertion" ;;
  esac
}

test_container_transport_linux() {
  run_container_check_linux transport
}

test_container_transport_darwin() {
  run_container_check_darwin transport
}

test_daemon_dispatch_latency() {
  case "$(uname -s)" in
    Linux) run_container_check_linux latency ;;
    Darwin) run_container_check_darwin latency ;;
    *) skip "unsupported platform: $(uname -s)" ;;
  esac
}

write_focus_fixture() {
  local runtime_dir="$1"
  local bin_dir="$2"
  local focus_target="$3"
  local safe_id
  local session_dir

  safe_id=$(printf '%s' "$focus_target" | LC_ALL=C tr -c 'A-Za-z0-9_-' '-')
  case "$(uname -s)" in
    Linux)
      session_dir="$runtime_dir/wrix/sessions"
      mkdir -p "$session_dir"
      jq -n --arg focus_target "$focus_target" --arg window_id "focused-window" \
        '{focus_target: $focus_target, window_id: $window_id}' >"$session_dir/$safe_id.json"
      cat >"$bin_dir/niri" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' '{"id":"focused-window"}'
EOF
      chmod +x "$bin_dir/niri"
      ;;
    Darwin)
      session_dir="$runtime_dir/data/wrix/sessions"
      mkdir -p "$session_dir"
      jq -n --arg focus_target "$focus_target" --arg terminal_app "FocusedTerminal" \
        '{focus_target: $focus_target, terminal_app: $terminal_app}' >"$session_dir/$safe_id.json"
      cat >"$bin_dir/osascript" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' 'FocusedTerminal'
EOF
      chmod +x "$bin_dir/osascript"
      ;;
    *) skip "unsupported platform: $(uname -s)" ;;
  esac

  cat >"$bin_dir/tmux" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exit 1
EOF
  chmod +x "$bin_dir/tmux"
}

write_host_focus_commands() {
  local bin_dir="$1"
  local command
  mkdir -p "$bin_dir"
  for command in niri osascript tmux; do
    cat >"$bin_dir/$command" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
state="${WRIX_NOTIFY_TEST_FOCUS_STATE:?}"
case "$(basename "$0")" in
  niri)
    [[ "$*" == 'msg -j focused-window' ]] || exit 91
    [[ ! -f "$state/fail-window" ]] || exit 1
    cat "$state/window"
    ;;
  osascript)
    [[ "$1" == '-e' ]] || exit 91
    [[ ! -f "$state/fail-window" ]] || exit 1
    cat "$state/app"
    ;;
  tmux)
    [[ "$*" == 'display-message -p #{session_name}:#{window_index}.#{pane_index}' ]] || exit 91
    [[ ! -f "$state/fail-pane" ]] || exit 1
    cat "$state/pane"
    ;;
esac
EOF
    chmod +x "$bin_dir/$command"
  done
}

start_client_daemon_relay() {
  local runtime_dir="$1"
  local port="$2"
  local log_file="$3"
  socat TCP-LISTEN:"$port",bind=127.0.0.1,fork,reuseaddr \
    UNIX-CONNECT:"$runtime_dir/wrix/notify.sock" >"$log_file" 2>&1 &
  BACKGROUND_PIDS+=("$!")
  wait_for_tcp_listener 127.0.0.1 "$port"
}

start_registered_launcher() {
  local directory="$1"
  local mode="$2"
  local runtime_dir="$3"
  local state="$4"
  local bin_dir="$5"
  local endpoint="$6"
  local title="${7:-$mode attention}"
  local -a args
  mkdir -p "$directory/workspace"
  if [[ "$mode" == "spawn" ]]; then
    args=(spawn --spawn-config "$directory/spawn.json")
  else
    args=(run "$directory/workspace" true)
  fi
  PATH="$bin_dir:$PATH" XDG_RUNTIME_DIR="$runtime_dir" XDG_DATA_HOME="$runtime_dir/data" \
    XDG_CACHE_HOME="$directory/cache" HOME="$directory/home" \
    WRIX_NOTIFY_TEST_FOCUS_STATE="$state" WRIX_TEST_DIGEST="sha256:$(printf 'a%.0s' {1..64})" \
    WRIX_TEST_CAPTURE="$directory/record.json" WRIX_NOTIFY_TEST_ENV_CAPTURE="$directory/env.json" \
    WRIX_NOTIFY_TEST_SESSION_DIR="$directory/sessions" WRIX_NOTIFY_TEST_CLIENT=1 \
    WRIX_NOTIFY_TEST_READY="$directory/ready" WRIX_NOTIFY_TEST_RELEASE="$directory/release" \
    WRIX_NOTIFY_TEST_FINISH="$directory/finish" \
    WRIX_NOTIFY_TEST_ENDPOINT="$endpoint" WRIX_NOTIFY_TEST_TITLE="$title" \
    WRIX_IMAGE_KEEP_FILE="$directory/mru.json" WRIX_GIT_SIGN=0 \
    WRIX_DEPLOY_KEY="$TEST_TMP/deploy-key" \
    timeout 15s "$WRIX_TEST_WRIX_BIN" --profile-config "$TEST_TMP/profile.json" "${args[@]}" \
    >"$directory/launch.log" 2>&1 &
  BACKGROUND_PIDS+=("$!")
}

wait_for_suppression_count() {
  local log_file="$1"
  local expected="$2"
  local attempt count
  for ((attempt = 0; attempt < 50; attempt += 1)); do
    count=$(awk '/suppressed \(terminal focused\)/ {count++} END {print count+0}' "$log_file")
    if [[ "$count" == "$expected" ]]; then return 0; fi
    sleep 0.1
  done
  return 1
}

assert_focus_runtime_fixture_conformance() {
  local bin_dir="$1"
  local command
  local rc
  for command in podman container niri osascript tmux route; do
    rc=0
    "$bin_dir/$command" unexpected arguments >"$TEST_TMP/fixture.out" 2>&1 || rc="$?"
    [[ "$rc" == "91" ]] || fail "external $command fixture accepted unsupported arguments"
  done
}

test_pi_settled() {
  require_command node
  local repo_root
  repo_root=$(resolve_repo_root)
  node --test --test-name-pattern='fixture conforms|settled' "$repo_root/tests/standalone/pi-notify-live.mjs"
}

test_pi_notify_failure() {
  require_command node
  local repo_root
  repo_root=$(resolve_repo_root)
  node --test --test-name-pattern='fixture conforms|unavailable transport' "$repo_root/tests/standalone/pi-notify-live.mjs"
}

test_pi_focus_routing() {
  ensure_tmp
  require_command node
  require_command jq
  require_command socat
  require_command nc
  [[ -x "${WRIX_TEST_WRIX_BIN:-}" ]] || fail "packaged launcher was not supplied"

  local source_kind
  case "$(uname -s)" in
    Linux) source_kind="nix-descriptor" ;;
    Darwin)
      source_kind="docker-archive"
      if ! ifconfig | grep 'inet 192.168.64.1 ' >/dev/null; then
        skip "Darwin vmnet gateway is unavailable; no host daemon transport was tested"
      fi
      ;;
    *) skip "unsupported notification platform" ;;
  esac
  local repo_root bin_dir target
  repo_root=$(resolve_repo_root)
  bin_dir="$TEST_TMP/host-bin"
  target=$' host/é:terminal\n'
  write_host_focus_commands "$bin_dir"
  cp "$repo_root/tests/standalone/notify-runtime.sh" "$bin_dir/podman"
  cp "$repo_root/tests/standalone/notify-runtime.sh" "$bin_dir/container"
  chmod +x "$bin_dir/podman" "$bin_dir/container"
  printf 'fixture key, not a credential\n' >"$TEST_TMP/deploy-key"
  jq -n --arg source_kind "$source_kind" --arg digest "sha256:$(printf 'a%.0s' {1..64})" '{
    schema: 1, system: "test", profile: {name: "base"}, agent: {kind: "pi"},
    image: {ref: "localhost/wrix-test:latest", source: "/missing/image", source_kind: $source_kind, digest: $digest},
    services: {nix_cache: {enable: false}}
  }' >"$TEST_TMP/profile.json"

  local case_name directory runtime_dir state capture daemon_log session_dir endpoint
  local daemon_pid relay_pid launcher_pid port
  for case_name in focused unfocused; do
    directory="$TEST_TMP/pi-$case_name"
    runtime_dir="$directory/runtime"
    state="$directory/focus-state"
    capture="$directory/dispatch.jsonl"
    daemon_log="$directory/daemon.log"
    mkdir -p "$state" "$directory/workspace" "$directory/home/.pi/agent"
    printf '{}\n' >"$directory/home/.pi/agent/auth.json"
    printf '{"id":42}\n' >"$state/window"
    printf 'FocusedTerminal\n' >"$state/app"
    printf 'host:2.1\n' >"$state/pane"
    case "$(uname -s)" in
      Linux) session_dir="$runtime_dir/wrix/sessions" ;;
      Darwin) session_dir="$runtime_dir/data/wrix/sessions" ;;
    esac
    ln -s "$session_dir" "$directory/sessions"
    jq -n --arg workspace "$directory/workspace" '{workspace: $workspace, env: [], agent_args: []}' >"$directory/spawn.json"
    PATH="$bin_dir:$PATH" WRIX_NOTIFY_TEST_FOCUS_STATE="$state" start_notify_daemon \
      "$runtime_dir" "$capture" "$daemon_log" 0 1 "" || fail_with_output "packaged daemon did not start" "$daemon_log"
    daemon_pid="${BACKGROUND_PIDS[-1]}"
    relay_pid=""
    if [[ "$(uname -s)" == "Linux" ]]; then
      port="$((42000 + (BASHPID % 20000)))"
      start_client_daemon_relay "$runtime_dir" "$port" "$directory/relay.log" || fail "client relay did not start"
      relay_pid="${BACKGROUND_PIDS[-1]}"
      endpoint="127.0.0.1:$port"
    else
      endpoint="$DARWIN_GATEWAY:$TCP_PORT"
    fi
    (
      unset WRIX_DRY_RUN WRIX_DRY_RUN_SERVICES WRIX_MICROVM WRIX_UNSAFE_PODMAN_SOCKET WRIX_SIGNING_KEY
      WRIX_FOCUS_TARGET="$target" TMUX=host-terminal \
        WRIX_NOTIFY_TEST_PI_RUNNER="$repo_root/tests/standalone/pi-notify-live.mjs" \
        PI_TEST_FOCUS_CAPTURE="$directory/wire.jsonl" \
        start_registered_launcher "$directory" spawn "$runtime_dir" "$state" "$bin_dir" "$endpoint" Pi
      launcher_pid="${BACKGROUND_PIDS[-1]}"
      wait_for_capture "$directory/record.json" || fail_with_output "Pi launcher did not register" "$directory/launch.log"
      jq -e --arg target "$target" '.focus_target == $target and .tmux_target == "host:2.1"' \
        "$directory/record.json" >/dev/null || fail "Pi launcher registered a different host target"
      if [[ "$case_name" == "unfocused" ]]; then
        printf '{"id":43}\n' >"$state/window"
        printf 'OtherTerminal\n' >"$state/app"
      fi
      touch "$directory/release"
      wait_for_capture "$directory/wire.jsonl" || fail_with_output "packaged Pi did not notify" "$directory/launch.log"
      assert_single_json_envelope "$directory/wire.jsonl"
      jq -e --arg target "$target" '.title == "Pi" and .message == "Waiting for input" and
        .focus_target == $target and (has("session_id") | not)' "$directory/wire.jsonl" >/dev/null || fail "Pi client envelope lost host routing or agent title"
      if [[ "$case_name" == "focused" ]]; then
        wait_for_suppression_count "$daemon_log" 1 || fail_with_output "Pi registered focus did not suppress" "$daemon_log"
        [[ ! -s "$capture" ]] || fail "focused Pi target dispatched"
      else
        wait_for_capture "$capture" || fail_with_output "Pi unfocused target did not dispatch" "$daemon_log"
        assert_native_dispatch "$capture" Pi 'Waiting for input' ""
      fi
      touch "$directory/finish"
      wait "$launcher_pid" || fail_with_output "Pi launch did not complete successfully" "$directory/launch.log"
    )
    stop_background_process "$daemon_pid"
    if [[ -n "$relay_pid" ]]; then stop_background_process "$relay_pid"; fi
  done
  pass "actual packaged Pi routes settled attention through registered launcher/client/daemon (external runtime/OS, not container delivery)"
}

test_focus_target_registration() {
  ensure_tmp
  require_command jq
  require_command socat
  require_command nc
  require_command timeout
  require_command wrix-notify
  require_command wrix-notifyd
  [[ -x "${WRIX_TEST_WRIX_BIN:-}" ]] || fail "packaged launcher was not supplied"

  local source_kind
  case "$(uname -s)" in
    Linux) source_kind="nix-descriptor" ;;
    Darwin)
      source_kind="docker-archive"
      if ! ifconfig | grep 'inet 192.168.64.1 ' >/dev/null; then
        skip "Darwin vmnet gateway is unavailable; no host daemon transport was tested"
      fi
      ;;
    *) skip "unsupported notification platform" ;;
  esac
  local bin_dir="$TEST_TMP/host-bin"
  local repo_root
  repo_root=$(resolve_repo_root)
  write_host_focus_commands "$bin_dir"
  cp "$repo_root/tests/standalone/notify-runtime.sh" "$bin_dir/podman"
  cp "$repo_root/tests/standalone/notify-runtime.sh" "$bin_dir/container"
  chmod +x "$bin_dir/podman" "$bin_dir/container"
  cat >"$bin_dir/route" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == '-n get default' ]] || exit 91
printf 'interface: en0\n'
EOF
  chmod +x "$bin_dir/route"
  WRIX_NOTIFY_TEST_FOCUS_STATE="$TEST_TMP" assert_focus_runtime_fixture_conformance "$bin_dir"
  printf 'fixture key, not a credential\n' >"$TEST_TMP/deploy-key"
  jq -n --arg source_kind "$source_kind" --arg digest "sha256:$(printf 'a%.0s' {1..64})" '{
    schema: 1, system: "test", profile: {name: "base"}, agent: {kind: "direct"},
    image: {ref: "localhost/wrix-test:latest", source: "/missing/image", source_kind: $source_kind, digest: $digest},
    services: {nix_cache: {enable: false}}
  }' >"$TEST_TMP/profile.json"

  local mode case_name directory runtime_dir state capture daemon_log session_dir target endpoint
  local daemon_pid relay_pid launcher_pid port second_directory second_pid colliding_target
  for mode in run spawn; do
    for case_name in focused opaque window-unfocused pane-unfocused pane-unknown pane-empty window-unknown window-missing absent empty; do
      directory="$TEST_TMP/$mode-$case_name"
      runtime_dir="$directory/runtime"
      state="$directory/focus-state"
      capture="$directory/dispatch.jsonl"
      daemon_log="$directory/daemon.log"
      mkdir -p "$state"
      printf '{"id":42}\n' >"$state/window"
      printf 'FocusedTerminal\n' >"$state/app"
      printf 'host:2.1\n' >"$state/pane"
      if [[ "$case_name" == "window-missing" ]]; then
        printf 'null\n' >"$state/window"
        : >"$state/app"
      fi
      case "$(uname -s)" in
        Linux) session_dir="$runtime_dir/wrix/sessions" ;;
        Darwin) session_dir="$runtime_dir/data/wrix/sessions" ;;
      esac
      ln -s "$session_dir" "$directory/sessions"
      target="host:2.1"
      case "$case_name" in
        opaque) target=$' opaque/é:terminal\n' ;;
        absent | empty) target="" ;;
      esac
      mkdir -p "$directory/workspace"
      jq -n --arg workspace "$directory/workspace" '{workspace: $workspace, env: [], agent_args: []}' >"$directory/spawn.json"
      if ! PATH="$bin_dir:$PATH" WRIX_NOTIFY_TEST_FOCUS_STATE="$state" start_notify_daemon \
        "$runtime_dir" "$capture" "$daemon_log" 0 1 ""; then
        fail_with_output "packaged daemon did not start" "$daemon_log"
      fi
      daemon_pid="${BACKGROUND_PIDS[-1]}"
      relay_pid=""
      if [[ "$(uname -s)" == "Linux" ]]; then
        port="$((42000 + (BASHPID % 20000)))"
        start_client_daemon_relay "$runtime_dir" "$port" "$directory/relay.log" || fail "client relay did not start"
        relay_pid="${BACKGROUND_PIDS[-1]}"
        endpoint="127.0.0.1:$port"
      else
        endpoint="$DARWIN_GATEWAY:$TCP_PORT"
      fi
      (
        export WRIX_SESSION_ID="legacy:9.9" WRIX_EXECUTION_ID="execution:9.9" PI_SESSION_ID="conversation:9.9"
        unset WRIX_DRY_RUN WRIX_DRY_RUN_SERVICES WRIX_MICROVM WRIX_UNSAFE_PODMAN_SOCKET WRIX_SIGNING_KEY
        case "$case_name" in
          absent) unset WRIX_FOCUS_TARGET TMUX ;;
          empty) export WRIX_FOCUS_TARGET="" TMUX=host-terminal ;;
          opaque) export WRIX_FOCUS_TARGET="$target" TMUX=host-terminal ;;
          *) unset WRIX_FOCUS_TARGET; export TMUX=host-terminal ;;
        esac
        start_registered_launcher "$directory" "$mode" "$runtime_dir" "$state" "$bin_dir" "$endpoint"
        launcher_pid="${BACKGROUND_PIDS[-1]}"
        if ! wait_for_capture "$directory/record.json"; then
          fail_with_output "launcher did not reach the runtime boundary" "$directory/launch.log"
        fi
        if [[ -n "$target" ]]; then
          jq -e --arg target "$target" '.focus_target == $target and .tmux_target == "host:2.1" and
            .registration_count == 1 and (has("session_id") | not)' "$directory/record.json" >/dev/null || fail "launcher registration mismatch"
          jq -e --arg pair "WRIX_FOCUS_TARGET=$target" 'map(select(startswith("WRIX_FOCUS_TARGET="))) == [$pair]' \
            "$directory/env.json" >/dev/null || fail "launcher did not export the exact registered target"
        else
          jq -e '. == null' "$directory/record.json" >/dev/null || fail "absent/empty focus unexpectedly registered"
          jq -e 'map(select(startswith("WRIX_FOCUS_TARGET="))) == []' "$directory/env.json" >/dev/null || fail "absent/empty focus exported"
        fi
        jq -e 'map(select(startswith("WRIX_SESSION_ID="))) == []' "$directory/env.json" >/dev/null || fail "launcher exported a session alias"
        if [[ "$case_name" == "focused" ]]; then
          second_directory="$directory/second"
          mkdir -p "$second_directory/workspace"
          ln -s "$session_dir" "$second_directory/sessions"
          jq -n --arg workspace "$second_directory/workspace" '{workspace: $workspace, env: [], agent_args: []}' >"$second_directory/spawn.json"
          start_registered_launcher "$second_directory" "$mode" "$runtime_dir" "$state" "$bin_dir" "$endpoint" "$mode overlap attention"
          second_pid="${BACKGROUND_PIDS[-1]}"
          wait_for_capture "$second_directory/record.json" || fail_with_output "overlapping launch did not register" "$second_directory/launch.log"
          jq -e '.registration_count == 2' "$second_directory/record.json" >/dev/null || fail "overlapping launch lost the shared registration"
        fi
        if [[ -n "$target" ]]; then
          colliding_target=$(printf '%s' "$target" | LC_ALL=C tr -c 'A-Za-z0-9_-' '-')
          WRIX_FOCUS_TARGET="$colliding_target" WRIX_NOTIFY_TCP="$endpoint" wrix-notify 'colliding target' 'not registered' Ping
          wait_for_capture "$capture" || fail "filename collision suppressed an unregistered target"
          assert_native_dispatch "$capture" 'colliding target' 'not registered' Ping
          : >"$capture"
          send_daemon_envelope "$runtime_dir" "$(jq -cn --arg target "$target" '{
            title: "legacy alias", message: "not focus", sound: "Ping", session_id: $target
          }')"
          wait_for_capture "$capture" || fail "daemon used legacy wire field as focus"
          assert_native_dispatch "$capture" 'legacy alias' 'not focus' Ping
          : >"$capture"
        fi
        case "$case_name" in
          window-unfocused) printf '{"id":43}\n' >"$state/window"; printf 'OtherTerminal\n' >"$state/app" ;;
          pane-unfocused) printf 'host:2.2\n' >"$state/pane" ;;
          pane-unknown) touch "$state/fail-pane" ;;
          pane-empty) : >"$state/pane" ;;
          window-unknown) touch "$state/fail-window" ;;
        esac
        touch "$directory/release"
        if [[ "$case_name" == "focused" || "$case_name" == "opaque" ]]; then
          wait_for_text "$daemon_log" 'suppressed (terminal focused)' || fail_with_output "positive focus did not suppress" "$daemon_log"
          [[ ! -s "$capture" ]] || fail "focused target dispatched"
        else
          wait_for_capture "$capture" || fail_with_output "unknown/unfocused target did not dispatch" "$daemon_log"
          assert_native_dispatch "$capture" "$mode attention" 'Waiting for input' Ping
        fi
        touch "$directory/finish"
        wait "$launcher_pid" || fail_with_output "packaged launch failed" "$directory/launch.log"
        if [[ "$case_name" == "focused" ]]; then
          jq -e '.registration_count == 1' "$session_dir/host-2-1.json" >/dev/null || fail "first cleanup removed a live shared registration"
          touch "$second_directory/release"
          wait_for_suppression_count "$daemon_log" 2 || fail_with_output "second registered launch did not suppress" "$daemon_log"
          [[ ! -s "$capture" ]] || fail "overlapping focused launch dispatched"
          touch "$second_directory/finish"
          wait "$second_pid" || fail_with_output "overlapping launch failed" "$second_directory/launch.log"
        fi
      )
      if [[ -d "$session_dir" ]] && compgen -G "$session_dir/*.json" >/dev/null; then
        fail "launcher did not remove its registration"
      fi
      : >"$capture"
      WRIX_FOCUS_TARGET="host:2.1" WRIX_NOTIFY_TCP="$endpoint" wrix-notify 'unknown target' 'registration gone' Ping
      wait_for_capture "$capture" || fail_with_output "missing registration suppressed delivery" "$daemon_log"
      assert_native_dispatch "$capture" 'unknown target' 'registration gone' Ping
      : >"$capture"
      send_daemon_envelope "$runtime_dir" '{"title":"legacy alias","message":"not focus","sound":"Ping","session_id":"host:2.1"}'
      wait_for_capture "$capture" || fail "legacy wire field prevented delivery"
      assert_native_dispatch "$capture" 'legacy alias' 'not focus' Ping
      stop_background_process "$daemon_pid"
      if [[ -n "$relay_pid" ]]; then stop_background_process "$relay_pid"; fi
    done
  done
  pass "packaged launcher/client/daemon agree on host routing with external OS/runtime fixtures (not container delivery)"
}

test_focus_override() {
  ensure_tmp
  require_command jq
  require_command nc
  require_command socat
  require_command wrix-notifyd

  local baseline_runtime="$TEST_TMP/focus-baseline"
  local baseline_capture="$TEST_TMP/focus-baseline.jsonl"
  local baseline_log="$TEST_TMP/focus-baseline.log"
  local override_runtime="$TEST_TMP/focus-override"
  local override_capture="$TEST_TMP/focus-override.jsonl"
  local override_log="$TEST_TMP/focus-override.log"
  local bin_dir="$TEST_TMP/focus-bin"
  local focus_target="focus-test:0.1"
  local payload
  local daemon_pid

  mkdir -p "$bin_dir"
  write_focus_fixture "$baseline_runtime" "$bin_dir" "$focus_target"
  payload=$(jq -cn --arg focus_target "$focus_target" \
    '{title: "focus override", message: "dispatch", sound: "Ping", focus_target: $focus_target}')

  if ! PATH="$bin_dir:$PATH" start_notify_daemon \
    "$baseline_runtime" "$baseline_capture" "$baseline_log" "0" "1" ""; then
    fail_with_output "could not start baseline focus daemon" "$baseline_log"
  fi
  daemon_pid="${BACKGROUND_PIDS[-1]}"
  send_daemon_envelope "$baseline_runtime" "$payload"
  if ! wait_for_text "$baseline_log" "notifyd: suppressed (terminal focused)"; then
    fail_with_output "baseline daemon did not positively identify the focused target" "$baseline_log"
  fi
  if [[ -s "$baseline_capture" ]]; then
    fail_with_output "baseline focused notification was not suppressed" "$baseline_capture"
  fi
  stop_background_process "$daemon_pid"

  write_focus_fixture "$override_runtime" "$bin_dir" "$focus_target"
  if ! PATH="$bin_dir:$PATH" start_notify_daemon \
    "$override_runtime" "$override_capture" "$override_log" "1" "0" ""; then
    fail_with_output "could not start focus-override daemon" "$override_log"
  fi
  send_daemon_envelope "$override_runtime" "$payload"
  if ! wait_for_capture "$override_capture"; then
    fail_with_output "WRIX_NOTIFY_ALWAYS did not bypass focus checking" "$override_log"
  fi
  pass "WRIX_NOTIFY_ALWAYS=1 disables focus checking"
}

test_verbose_logging() {
  ensure_tmp
  require_command jq
  require_command nc
  require_command socat
  require_command wrix-notify
  require_command wrix-notifyd

  local client_log="$TEST_TMP/verbose-client.log"
  local runtime_dir="$TEST_TMP/verbose-runtime"
  local capture="$TEST_TMP/verbose-dispatch.jsonl"
  local daemon_log="$TEST_TMP/verbose-daemon.log"
  local payload

  WRIX_NOTIFY_TCP="invalid-endpoint" WRIX_NOTIFY_VERBOSE=1 \
    wrix-notify "verbose client" "invalid endpoint" >"$client_log" 2>&1
  if [[ "$(<"$client_log")" != *"wrix-notify: invalid TCP endpoint: invalid-endpoint"* ]]; then
    fail_with_output "WRIX_NOTIFY_VERBOSE did not enable client diagnostics" "$client_log"
  fi

  if ! start_notify_daemon "$runtime_dir" "$capture" "$daemon_log" "0" "1" ""; then
    fail_with_output "could not start verbose notification daemon" "$daemon_log"
  fi
  payload=$(jq -cn \
    '{title: "verbose daemon", message: "missing target", sound: "Ping", focus_target: "missing:0.1"}')
  send_daemon_envelope "$runtime_dir" "$payload"
  if ! wait_for_capture "$capture"; then
    fail_with_output "verbose daemon did not dispatch the test payload" "$daemon_log"
  fi
  if [[ "$(<"$daemon_log")" != *"notifyd: session file not found:"* ]]; then
    fail_with_output "WRIX_NOTIFY_VERBOSE did not enable daemon diagnostics" "$daemon_log"
  fi
  pass "WRIX_NOTIFY_VERBOSE=1 enables notification diagnostics"
}

main() {
  local test_name="${1:-}"

  if [[ -z "$test_name" ]]; then
    case "$(uname -s)" in
      Linux) test_name="test_container_transport_linux" ;;
      Darwin) test_name="test_container_transport_darwin" ;;
      *) skip "unsupported platform: $(uname -s)" ;;
    esac
  fi

  case "$test_name" in
    --inside-container) test_container_payload_inside ;;
    test_focus_target_envelope) test_focus_target_envelope ;;
    test_focus_target_registration) test_focus_target_registration ;;
    test_pi_settled) test_pi_settled ;;
    test_pi_focus_routing) test_pi_focus_routing ;;
    test_pi_notify_failure) test_pi_notify_failure ;;
    test_client_non_blocking) test_client_non_blocking ;;
    test_client_tcp_endpoint_override) test_client_tcp_endpoint_override ;;
    test_container_transport_darwin) test_container_transport_darwin ;;
    test_container_transport_linux) test_container_transport_linux ;;
    test_daemon_dispatch_latency) test_daemon_dispatch_latency ;;
    test_focus_override) test_focus_override ;;
    test_verbose_logging) test_verbose_logging ;;
    *) fail "unknown notify test: $test_name" ;;
  esac
}

main "$@"
