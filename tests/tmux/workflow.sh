#!/usr/bin/env bash
# Guest shells, not the host shell, expand the literal command snippets.
# shellcheck disable=SC2016
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"
wrix_require_live_sandbox_linux
cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-tmux-workflow.XXXXXX)
IMAGE_REFS=()
WORKSPACES=()
LAUNCH_PIDS=()
cleanup() {
  local ref cid pid status workspace
  for workspace in "${WORKSPACES[@]}"; do
    if cid=$(find_sandbox "$workspace") && [[ -n "$cid" ]]; then
      timeout --kill-after=2 10 podman rm -f --time 2 "$cid" >&2 || true # best-effort: preserve test failure if the container already exited.
    fi
  done
  for pid in "${LAUNCH_PIDS[@]}"; do
    if kill -0 "$pid" 2>/dev/null; then
      kill "$pid" || true # best-effort: launcher may exit between probe and signal.
    fi
    if wait "$pid"; then :; else status=$?; printf 'cleanup: launcher exited %s\n' "$status" >&2; fi
  done
  for workspace in "${WORKSPACES[@]}"; do
    (cd "$workspace" && HOME="$TEST_TMP/home" XDG_CACHE_HOME="$TEST_TMP/cache" \
      timeout --kill-after=2 10 "$PACKAGE/bin/wrix" service stop) || true # best-effort: service startup may have failed before creation.
  done
  for ref in "${IMAGE_REFS[@]}"; do
    if timeout --kill-after=2 10 podman image exists "$ref"; then
      timeout --kill-after=2 10 podman rmi "$ref" >&2 || true # best-effort: preserve verifier status if runtime image cleanup fails.
    fi
  done
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

PACKAGE=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-tmux-sandbox)
PROFILE_CONFIG=$(nix eval --raw --no-warn-dirty .#test-tmux-sandbox.profileConfig)
mkdir -p "$TEST_TMP/home" "$TEST_TMP/cache"

wait_for() {
  local attempt
  for ((attempt = 0; attempt < ${WAIT_ATTEMPTS:-50}; attempt++)); do
    if "$@"; then return 0; fi
    sleep 0.1
  done
  printf 'Timed out: %s\n' "$*" >&2
  return 1
}

find_sandbox() {
  local workspace="$1" ids
  local -a containers
  ids=$(timeout --kill-after=2 10 podman ps -q)
  [[ -n "$ids" ]] || return 0
  mapfile -t containers <<<"$ids"
  timeout --kill-after=2 10 podman inspect "${containers[@]}" | jq -r --arg workspace "$workspace" \
    '.[] | select(any(.Mounts[]; .Destination == "/workspace" and .Source == $workspace)) | .Id'
}

start_sandbox() {
  local name="$1" workspace="$TEST_TMP/$1" ref spawn_config pid
  ref="localhost/wrix-tmux-$name-$$:test"
  IMAGE_REFS+=("$ref")
  spawn_config="$TEST_TMP/$name.json"
  mkdir -p "$workspace"
  WORKSPACES+=("$workspace")
  wrix_write_spawn_config "$spawn_config" "$workspace" bash -c \
    'set -euo pipefail; test -f /etc/wrix/tmux.md; [[ $(jq ".servers | length" "$WRIX_MCP_MANIFEST") == 0 ]]; if command -v tmux-mcp; then exit 1; fi; touch /workspace/ready; exec sleep 150'
  jq --arg ref "$ref" --slurpfile profile "$PROFILE_CONFIG" \
    '.image_ref = $ref | .image_source = $profile[0].image.source | .image_source_kind = $profile[0].image.source_kind | .git = {deploy: false, sign: false}' \
    "$spawn_config" >"$spawn_config.tmp"
  mv "$spawn_config.tmp" "$spawn_config"
  HOME="$TEST_TMP/home" XDG_CACHE_HOME="$TEST_TMP/cache" WRIX_MICROVM=0 \
    timeout --kill-after=5 180 "$PACKAGE/bin/wrix" spawn --spawn-config "$spawn_config" >"$TEST_TMP/$name.log" 2>&1 &
  pid=$!
  LAUNCH_PIDS+=("$pid")
  if ! WAIT_ATTEMPTS=1200 wait_for test -f "$workspace/ready"; then
    printf '%s\n' "$(<"$TEST_TMP/$name.log")" >&2
    return 1
  fi
  find_sandbox "$workspace" >"$TEST_TMP/$name.cid"
  [[ $(wc -l <"$TEST_TMP/$name.cid") == 1 ]]
}

run_in() {
  local name="$1"
  shift
  timeout --kill-after=2 10 podman exec -w /workspace "$(<"$TEST_TMP/$name.cid")" bash -c 'set -euo pipefail; eval "$1"' probe "$1"
}

create_session() {
  local name="$1"
  run_in "$name" '
    workflow_dir=$(mktemp -d /tmp/wrix-tmux.XXXXXX)
    socket="$workflow_dir/socket"
    pane=$(tmux -S "$socket" new-session -d -s debug -P -F "#{pane_id}" "bash --noprofile --norc")
    printf "%s\n%s\n" "$socket" "$pane" > /workspace/targets
    tmux -S "$socket" list-sessions -F "#{session_name}" | grep -x debug
    tmux -S "$socket" list-panes -a -F "#{pane_id}" | grep -Fx "$pane"
    tmux -S "$socket" set-option -w -t "$pane" remain-on-exit on
  '
}

pane_call() {
  local name="$1" command="$2"
  run_in "$name" 'mapfile -t targets < /workspace/targets; socket="${targets[0]}"; pane="${targets[1]}"; '"$command"
}

pane_has_output() {
  local name="$1" text="$2" output
  output=$(pane_call "$name" 'tmux -S "$socket" capture-pane -p -t "$pane" -S -1000')
  [[ "$output" == *"$text"* ]]
}

pane_status_is() {
  local name="$1" expected="$2" status
  status=$(pane_call "$name" 'tmux -S "$socket" display-message -p -t "$pane" "#{pane_dead}:#{pane_dead_status}"')
  [[ "$status" == "$expected" ]]
}

pane_command_is() {
  local name="$1" expected="$2" command
  command=$(pane_call "$name" 'tmux -S "$socket" display-message -p -t "$pane" "#{pane_current_command}"')
  [[ "$command" == "$expected" ]]
}

request_server() {
  local name="$1" output
  if output=$(run_in "$name" 'curl --fail --silent --show-error --max-time 2 http://127.0.0.1:8765/served.txt'); then
    [[ "$output" == served-by-native-tmux ]]
  else
    return 1
  fi
}

test_cli_workflow() {
  start_sandbox workflow
  create_session workflow
  pane_call workflow 'printf "served-by-native-tmux\n" > served.txt; tmux -S "$socket" respawn-pane -k -t "$pane" "python3 -u -m http.server 8765 --bind 127.0.0.1"'
  wait_for request_server workflow
  wait_for pane_has_output workflow 'GET /served.txt'
  pane_status_is workflow '0:'
  pane_call workflow 'tmux -S "$socket" respawn-pane -k -t "$pane" "env PS1=READY_PROMPT bash --noprofile --norc"'
  wait_for pane_has_output workflow READY_PROMPT
  pane_call workflow 'tmux -S "$socket" send-keys -t "$pane" -l -- "printf '\''literal Enter C-c\\n'\'' > /workspace/literal.txt"'
  run_in workflow '[[ ! -e /workspace/literal.txt ]]'
  pane_call workflow 'tmux -S "$socket" send-keys -t "$pane" Enter'
  wait_for run_in workflow '[[ -f /workspace/literal.txt && $(</workspace/literal.txt) == "literal Enter C-c" ]]'
  pane_call workflow 'tmux -S "$socket" send-keys -t "$pane" -l -- "sleep 100"; tmux -S "$socket" send-keys -t "$pane" Enter'
  wait_for pane_command_is workflow sleep
  pane_call workflow 'tmux -S "$socket" send-keys -t "$pane" C-c; tmux -S "$socket" send-keys -t "$pane" -l -- "touch /workspace/interrupt-returned"; tmux -S "$socket" send-keys -t "$pane" Enter'
  wait_for run_in workflow '[[ -e /workspace/interrupt-returned ]]'
}

test_cli_exited_process() {
  start_sandbox exited
  create_session exited
  pane_status_is exited '0:'
  pane_call exited 'tmux -S "$socket" respawn-pane -k -t "$pane" "bash -c '\''printf \"fast failure\\n\"; exit 7'\''"'
  wait_for pane_status_is exited '1:7'
  pane_has_output exited 'fast failure'
  pane_call exited 'tmux -S "$socket" list-panes -a -F "#{pane_id} dead=#{pane_dead} exit=#{pane_dead_status}" | grep -Fx "$pane dead=1 exit=7"'
}

test_cli_targeted_cleanup() {
  start_sandbox targeted
  create_session targeted
  pane_call targeted 'tmux -S "$socket" new-session -d -s unrelated "sleep 100"; tmux -S "$socket" kill-session -t "=debug"; if tmux -S "$socket" has-session -t "=debug"; then exit 1; fi; tmux -S "$socket" has-session -t "=unrelated"; tmux -S "$socket" list-sessions -F "#{session_name}" | grep -x unrelated'
}

host_processes_gone() {
  local pid
  for pid in "$@"; do
    if kill -0 "$pid" 2>/dev/null; then return 1; fi
  done
}

test_cli_container_cleanup() {
  local cid processes pid
  local -a host_pids
  start_sandbox stopped
  start_sandbox survivor
  create_session stopped
  create_session survivor
  pane_call stopped 'tmux -S "$socket" respawn-pane -k -t "$pane" "sleep 100"'
  pane_call survivor 'tmux -S "$socket" respawn-pane -k -t "$pane" "sleep 100"'
  cid=$(<"$TEST_TMP/stopped.cid")
  processes=$(timeout --kill-after=2 10 podman top "$cid" hpid args)
  [[ "$processes" == *tmux* && "$processes" == *'sleep 100'* ]]
  mapfile -t host_pids < <(awk 'NR > 1 { print $1 }' <<<"$processes")
  [[ "${#host_pids[@]}" -gt 2 ]]
  for pid in "${host_pids[@]}"; do kill -0 "$pid"; done
  timeout --kill-after=2 10 podman stop --time 2 "$cid"
  wait_for host_processes_gone "${host_pids[@]}"
  [[ $(timeout --kill-after=2 10 podman ps -q --filter "id=$cid") == "" ]]
  pane_status_is survivor '0:'
  pane_call survivor 'tmux -S "$socket" capture-pane -p -t "$pane" -S -1000; kill -0 "$(tmux -S "$socket" display-message -p -t "$pane" "#{pane_pid}")"'
}

case "${1:?test function required}" in
  test_cli_workflow) test_cli_workflow ;;
  test_cli_exited_process) test_cli_exited_process ;;
  test_cli_targeted_cleanup) test_cli_targeted_cleanup ;;
  test_cli_container_cleanup) test_cli_container_cleanup ;;
  *) printf 'Unknown test: %s\n' "$1" >&2; exit 64 ;;
esac
