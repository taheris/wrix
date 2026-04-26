#!/usr/bin/env bash
# Gas City exec:<script> provider — translates gc commands to podman operations.
#
# Called by gc as: provider.sh <method> <session-name> [args...]
#
# Environment variables (set by mkCity / entrypoint):
#   GC_CITY_NAME    — city name for container labeling
#   GC_WORKSPACE    — host workspace path (mounted into containers)
#   GC_AGENT_IMAGE  — OCI image for agent containers
#   GC_PODMAN_NETWORK — podman network name (wrapix-<city>)
set -euo pipefail

METHOD="${1:?missing method}"
SESSION="${2:-}"
shift 2 || shift $#

# gc's exec provider sends data on stdin for some methods (start, nudge,
# set-meta, process-alive). Read stdin once and store it.
STDIN_DATA=""
if [[ "$METHOD" == "start" || "$METHOD" == "nudge" || "$METHOD" == "set-meta" || "$METHOD" == "process-alive" ]]; then
  STDIN_DATA="$(cat)"
fi


# ---------------------------------------------------------------------------
# Environment contract
#
# Every variable listed here is required for container start (persistent or
# worker). The check-env method validates them — called by unit tests against
# both the shellHook and entrypoint env sets. Add new requirements here, not
# as ad-hoc ${VAR:?} scattered through the code.
# ---------------------------------------------------------------------------

REQUIRED_ENV=(
  GC_CITY_NAME
  GC_WORKSPACE
  GC_AGENT_IMAGE
  GC_PODMAN_NETWORK
  GC_BEADS_DOLT_CONTAINER
  GC_DOLT_PORT
)

check_env() {
  local fail=0
  for var in "${REQUIRED_ENV[@]}"; do
    if [[ -z "${!var:-}" ]]; then
      echo "MISSING: $var" >&2
      fail=1
    fi
  done
  return "$fail"
}

# ---------------------------------------------------------------------------
# Host-path translation (nested containers via host podman socket)
# ---------------------------------------------------------------------------

# Rewrite a container-local path to its host-side equivalent.
# When GC_HOST_WORKSPACE is set (container talking to host podman),
# paths under $GC_WORKSPACE are translated so the host daemon can
# resolve bind mounts.  .beads has a separate overlay (GC_HOST_BEADS).
# No-op when GC_HOST_WORKSPACE is unset (normal host case).
_host_path() {
  local p="$1"
  [[ -n "${GC_HOST_WORKSPACE:-}" ]] || { echo "$p"; return; }
  if [[ -n "${GC_HOST_BEADS:-}" && "$p" == "${GC_WORKSPACE}/.beads"* ]]; then
    echo "${GC_HOST_BEADS}${p#${GC_WORKSPACE}/.beads}"
    return
  fi
  if [[ "$p" == "${GC_WORKSPACE}"* ]]; then
    echo "${GC_HOST_WORKSPACE}${p#${GC_WORKSPACE}}"
    return
  fi
  echo "$p"
}

# Volume flags for persistent containers (scout, judge, mayor).
container_volumes_persistent() {
  local ws_mode="$1" beads_staging="$2"
  echo "-v $(_host_path "${GC_WORKSPACE}"):/workspace:${ws_mode}"
  echo "-v $(_host_path "${beads_staging}"):/workspace/.beads"
  echo "-v $(_host_path "${GC_WORKSPACE}/.gc"):/workspace/.gc:rw"
  echo "-v $(_host_path "${GC_WORKSPACE}/.wrapix"):/workspace/.wrapix:rw"
  echo "-v $(_host_path "${GC_WORKSPACE}/.claude"):/workspace/.claude:rw"
}

# Volume flags for worker containers.
container_volumes_worker() {
  local worktree_path="$1" beads_staging="$2" task_file="$3" log_dir="$4"
  echo "-v $(_host_path "${GC_WORKSPACE}/${worktree_path}"):/workspace:rw"
  echo "-v $(_host_path "${GC_WORKSPACE}/.git"):/mnt/git:rw"
  echo "-v $(_host_path "${GC_WORKSPACE}/.wrapix"):/workspace/.wrapix:ro"
  echo "-v $(_host_path "${beads_staging}"):/workspace/.beads"
  echo "-v $(_host_path "${task_file}"):/workspace/.task:ro"
  echo "-v $(_host_path "${log_dir}"):/workspace/logs:rw"
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

container_name() {
  local prefix="${GC_CITY_NAME:?}-"
  # Avoid double-prefixing: gc passes bare names on first start
  # ("mayor") but fully-qualified names after config reload ("dev-mayor").
  if [[ "${SESSION:?}" == "${prefix}"* ]]; then
    echo "${SESSION}"
  else
    echo "${prefix}${SESSION}"
  fi
}

# Detect worker sessions.  gc may assign session names that don't contain
# "worker" (e.g. bead-id based names in convergences).  Check the start
# data's agent_template field first, then fall back to name patterns. (wx-aqe4z)
is_worker() {
  # Fast path: name-based detection
  if [[ "${SESSION}" == worker* || "${SESSION}" == *-worker* ]]; then
    return 0
  fi
  # Parse agent_template from gc's start JSON on stdin.
  if [[ -n "${STDIN_DATA:-}" ]]; then
    local tpl
    tpl="$(echo "$STDIN_DATA" | grep -o '"agent_template" *: *"[^"]*"' | head -1 | grep -o '"[^"]*"$' | tr -d '"')" || tpl=""
    if [[ "$tpl" == "worker" ]]; then
      return 0
    fi
  fi
  # Check GC_AGENT_TEMPLATE env var (set by our own start for sub-calls)
  if [[ "${GC_AGENT_TEMPLATE:-}" == "worker" ]]; then
    return 0
  fi
  # Known non-worker template — skip the slow podman inspect fallback.
  if [[ -n "${GC_AGENT_TEMPLATE:-}" ]]; then
    return 1
  fi
  # Container label fallback — covers non-start methods (stop, is-running, peek)
  # for s-wx-* session names where name patterns don't match. (wx-pq03c)
  local _gc_label
  _gc_label="$(podman inspect --format '{{index .Config.Labels "gc-role"}}' "$(container_name)" 2>/dev/null)" || _gc_label=""
  [[ "$_gc_label" == "worker" ]]
}

is_judge() {
  [[ "${SESSION}" == judge* || "${SESSION}" == *-judge* ]]
}

is_mayor() {
  [[ "${SESSION}" == mayor* || "${SESSION}" == *-mayor* ]]
}

# Base role name (mayor, scout, judge, worker) regardless of session prefix.
# Checks GC_AGENT_TEMPLATE first (set during start), then name patterns.
is_scout() {
  [[ "${SESSION}" == scout* || "${SESSION}" == *-scout* ]]
}

role_name() {
  if [[ "${GC_AGENT_TEMPLATE:-}" == "worker" ]] || is_worker; then echo "worker"
  elif is_mayor; then echo "mayor"
  elif is_judge; then echo "judge"
  elif is_scout; then echo "scout"
  elif [[ -n "${GC_AGENT_TEMPLATE:-}" ]]; then echo "${GC_AGENT_TEMPLATE}"
  else
    # Container label fallback for roles with non-standard session names (wx-pq03c)
    local _gc_label
    _gc_label="$(podman inspect --format '{{index .Config.Labels "gc-role"}}' "$(container_name)" 2>/dev/null)" || _gc_label=""
    if [[ -n "$_gc_label" ]]; then
      echo "$_gc_label"
    else
      echo "role_name: cannot determine role for session '${SESSION}'" >&2
      return 1
    fi
  fi
}

# Shared tmux socket path for a target role.
# Persistent containers create sockets here so any container (or the host)
# can reach another role's tmux without podman exec.
tmux_sock() {
  local target="${1:-$(role_name)}"
  local ws="${GC_WORKSPACE:-/workspace}"
  echo "${ws}/.wrapix/tmux/${target}.sock"
}

# Run a tmux command against a role's shared socket. On Linux, uses the
# socket directly from the host. On Darwin, VirtioFS exposes the socket
# file but can't relay Unix domain socket IPC, so routes through podman exec.
tmux_via() {
  local target="$1"
  shift
  local sock
  sock="$(tmux_sock "$target")"
  if [[ -S "$sock" ]] && [[ -n "${GC_AGENT:-}" || "$(uname)" != "Darwin" ]]; then
    tmux -S "$sock" "$@"
  else
    podman exec "$(container_name)" tmux -S "/workspace/.wrapix/tmux/${target}.sock" "$@"
  fi
}

# Stage .beads config files for container-local database isolation.
# Each container gets its own .beads with just config — no host mount.
stage_beads() {
  local staging
  staging="${GC_WORKSPACE}/.gc/beads-staging/$(container_name)"
  rm -rf "$staging"
  mkdir -p "$staging"
  chmod 700 "$staging"
  local beads="${GC_WORKSPACE}/.beads"
  [ -f "$beads/config.yaml" ] && cp "$beads/config.yaml" "$staging/"
  [ -f "$beads/metadata.json" ] && cp "$beads/metadata.json" "$staging/"
  [ -f "$beads/issues.jsonl" ] && cp "$beads/issues.jsonl" "$staging/"
  echo "$staging"
}

# Standard labels applied to every container
container_labels() {
  local role
  if is_worker; then
    role="worker"
  elif [[ "${SESSION}" == scout* || "${SESSION}" == *-scout* ]]; then
    role="scout"
  elif is_judge; then
    role="judge"
  elif is_mayor; then
    role="mayor"
  else
    role="${SESSION}"
  fi

  echo "--label=gc-city=${GC_CITY_NAME}"
  echo "--label=gc-role=${role}"
  if is_worker && [[ -n "${GC_BEAD_ID:-}" ]]; then
    echo "--label=gc-bead=${GC_BEAD_ID}"
  fi
}

# Resource limit flags for a role
resource_flags() {
  local role="$1"
  local flags=""
  if [[ -n "${GC_CPUS:-}" ]]; then
    flags+=" --cpus=${GC_CPUS}"
  fi
  if [[ -n "${GC_MEMORY:-}" ]]; then
    flags+=" --memory=${GC_MEMORY}"
  fi
  echo "$flags"
}

# Common env flags for all containers (persistent and worker).
# Centralizes the gc→container env contract — add new vars here, not
# in each start function.  Uses the same echo-flags pattern as
# container_labels() and resource_flags().
#
# bd inside the container reaches dolt through the mounted Unix socket
# at /workspace/.wrapix/dolt.sock. GC_DOLT_HOST/PORT remain for gc's
# own TCP fallback but bd itself uses the socket exclusively.
container_env() {
  local dolt_host="$1" dolt_port="$2"
  echo "-e BEADS_DOLT_AUTO_START=0"
  echo "-e BEADS_DOLT_SERVER_SOCKET=/workspace/.wrapix/dolt.sock"
  echo "-e GC_DOLT_HOST=${dolt_host}"
  echo "-e GC_DOLT_PORT=${dolt_port}"
  echo "-e CLAUDE_CODE_OAUTH_TOKEN=${CLAUDE_CODE_OAUTH_TOKEN:-}"
  echo "-e GC_CITY_NAME=${GC_CITY_NAME}"
  echo "-e GC_CITY=/workspace/.gc/home"
  echo "-e WRAPIX_CITY_DIR=/workspace/.wrapix/city/current"
  echo "-e HOME=/home/wrapix"
  echo "-e TERM=xterm-256color"
}

# ---------------------------------------------------------------------------
# Persistent role helpers (scout, judge) — tmux as PID 1
# ---------------------------------------------------------------------------

persistent_start() {
  local name ws_mode
  name="$(container_name)"

  # Judge needs read-write workspace access for merge operations;
  # other persistent roles (scout, mayor) get read-only.
  if is_judge; then
    ws_mode="rw"
  else
    ws_mode="ro"
  fi

  # bd inside the container reaches dolt via the mounted Unix socket
  # (/workspace/.wrapix/dolt.sock). dolt_host/dolt_port are passed to
  # container_env for gc's direct TCP fallback only.
  local dolt_host="${GC_BEADS_DOLT_CONTAINER:?provider requires GC_BEADS_DOLT_CONTAINER}"
  local dolt_port="${GC_DOLT_PORT:?provider requires GC_DOLT_PORT}"

  local role beads_staging
  role="$(role_name)"
  beads_staging="$(stage_beads)"

  # shellcheck disable=SC2046,SC2086
  podman run -d \
    --pull=never \
    --replace \
    --name "$name" \
    --entrypoint "" \
    --network "${GC_PODMAN_NETWORK:?}" \
    --userns=keep-id \
    --passwd-entry "wrapix:*:$(id -u):$(id -g)::/home/wrapix:/bin/bash" \
    --mount type=tmpfs,destination=/home/wrapix,U=true \
    --mount type=tmpfs,destination=/tmp,U=true \
    --workdir /workspace \
    $(container_labels) \
    $(resource_flags "${SESSION}") \
    $(container_volumes_persistent "$ws_mode" "$beads_staging") \
    ${GC_SECRET_FLAGS:-} \
    $(container_env "$dolt_host" "$dolt_port") \
    -e "GC_SESSION=exec:/workspace/.gc/scripts/provider.sh" \
    -e "GC_AGENT=${role}" \
    -e "GC_ALIAS=${role}" \
    "${GC_AGENT_IMAGE:?}" \
    bash -c '
      set -e
      # shellcheck disable=SC1091
      [[ -f /git-ssh-setup.sh ]] && . /git-ssh-setup.sh
      mkdir -p "$HOME/.claude"
      cp /etc/wrapix/claude-config.json "$HOME/.claude.json"
      # Merge Nix-generated settings (env, Notification hook) with gc-
      # installed hooks (UserPromptSubmit, Stop).  gc owns the hook
      # definitions — using its template keeps hooks in sync across
      # gc version upgrades.
      if [[ -f /workspace/.gc/home/hooks/claude.json ]]; then
        jq -s "(.[0] * .[1]) + {hooks: ((.[0].hooks // {}) + (.[1].hooks // {}))}" \
          "$WRAPIX_CITY_DIR/claude-settings.json" \
          /workspace/.gc/home/hooks/claude.json \
          > "$HOME/.claude/settings.json"
      else
        cp "$WRAPIX_CITY_DIR/claude-settings.json" "$HOME/.claude/settings.json"
      fi
      cp "$WRAPIX_CITY_DIR/tmux.conf" "$HOME/.tmux.conf"
      mkdir -p /workspace/.wrapix/tmux
      _sock="/workspace/.wrapix/tmux/${GC_AGENT}.sock"
      tmux -S "$_sock" start-server
      tmux -S "$_sock" new-session -d -s "$GC_AGENT" "claude --dangerously-skip-permissions"
      exec tmux -S "$_sock" wait-for gc-shutdown
    '

  # Verify the container survived initialization (tmux startup).
  # podman run -d returns before the inline script executes, so a brief
  # wait lets tmux either start or fail-and-exit.
  sleep 2
  if [[ "$(podman inspect --format '{{.State.Running}}' "$name" 2>/dev/null)" != "true" ]]; then
    echo "persistent_start: container $name exited during startup — check tmux/config" >&2
    podman logs --tail 20 "$name" 2>&1 >&2 || true
    return 1
  fi
}

persistent_exec() {
  local target
  target="$(role_name)"
  shift  # drop "tmux" (always the first arg at every call site)
  tmux_via "$target" "$@"
}

# ---------------------------------------------------------------------------
# Ephemeral worker helpers — task via tmux (wx-m5sd6)
# ---------------------------------------------------------------------------

worker_start() {
  local name bead_id worktree_path
  name="$(container_name)"

  # Pre-start guard: avoid killing an in-progress worker (wx-tvj7o)
  local _existing_state
  _existing_state="$(podman inspect --format '{{.State.Status}}' "$name" 2>/dev/null)" || _existing_state=""
  case "$_existing_state" in
    running)
      echo "worker_start: container $name already running — skipping duplicate start" >&2
      return 0
      ;;
    exited|stopped|created)
      podman rm "$name" 2>/dev/null || true
      ;;
  esac

  # Resolve script directory (same dir as this provider.sh)
  local script_dir
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

  # Resolve bead ID if not already set (reconciler path doesn't pass issue
  # in start JSON; worker-setup.sh falls back to bd list routed beads).
  if [[ -z "${GC_BEAD_ID:-}" ]]; then
    # best-effort: bd may not have routed beads yet
    GC_BEAD_ID="$(cd "${GC_WORKSPACE}" && bd list --metadata-field gc.routed_to=worker \
      --status open,in_progress --json 2>/dev/null \
      | jq -r '.[0].id // empty' 2>/dev/null)" || GC_BEAD_ID=""
    if [[ -z "$GC_BEAD_ID" ]]; then
      echo "worker start: no bead routed to worker" >&2
      return 1
    fi
    export GC_BEAD_ID
  fi

  # Worker setup: worktree creation, bead claiming, task file generation.
  # Shared with integration tests via worker-setup.sh.
  bash "${script_dir}/worker-setup.sh" >/dev/null || {
    echo "worker start: setup failed for bead ${GC_BEAD_ID}" >&2
    return 1
  }
  bead_id="${GC_BEAD_ID}"
  worktree_path=".wrapix/worktree/${bead_id}"

  # Host-side persistent log directory — survives worktree cleanup (wx-iy1vt)
  local host_log_dir="${GC_WORKSPACE}/.gc/logs/worker/${bead_id}"
  mkdir -p "$host_log_dir"

  # is-running reads .wrapix/state/worker-<bead>.done to distinguish "monitor
  # still running post-gate" from "fully done, safe to dispose" (wx-92md7).
  # Clear any stale marker from a prior run of the same bead before starting.
  local state_dir="${GC_WORKSPACE}/.wrapix/state"
  local done_marker="${state_dir}/worker-${bead_id}.done"
  mkdir -p "$state_dir"
  rm -f "$done_marker"

  local task_file="${GC_WORKSPACE}/${worktree_path}/.task"

  # Rewrite worktree .git reference for container-internal mount path.
  # The host worktree's .git file points to <host-abs>/.git/worktrees/gc-<id>,
  # which doesn't exist inside the container. Mount the main .git at /mnt/git
  # and rewrite the gitdir to match.
  echo "gitdir: /mnt/git/worktrees/${bead_id}" > "${GC_WORKSPACE}/${worktree_path}/.git"

  # bd inside the worker reaches dolt via the mounted Unix socket
  # (/workspace/.wrapix/dolt.sock). dolt_host/dolt_port are passed to
  # container_env for gc's direct TCP fallback only.
  local dolt_host="${GC_BEADS_DOLT_CONTAINER:?provider requires GC_BEADS_DOLT_CONTAINER}"
  local dolt_port="${GC_DOLT_PORT:?provider requires GC_DOLT_PORT}"
  local beads_staging
  beads_staging="$(stage_beads)"

  # shellcheck disable=SC2046,SC2086
  podman run -d \
    --pull=never \
    --name "$name" \
    --entrypoint "" \
    --network "${GC_PODMAN_NETWORK:?}" \
    --userns=keep-id \
    --passwd-entry "wrapix:*:$(id -u):$(id -g)::/home/wrapix:/bin/bash" \
    --mount type=tmpfs,destination=/home/wrapix,U=true \
    --mount type=tmpfs,destination=/tmp,U=true \
    --workdir /workspace \
    $(container_labels) \
    $(resource_flags worker) \
    $(container_volumes_worker "$worktree_path" "$beads_staging" "$task_file" "$host_log_dir") \
    ${GC_SECRET_FLAGS:-} \
    $(container_env "$dolt_host" "$dolt_port") \
    -e "GC_BEAD_ID=${bead_id}" \
    -e "GC_SESSION=worker" \
    -e "GC_AGENT=worker" \
    -e "WRAPIX_PROMPT_FILE=/workspace/.task" \
    -e "WRAPIX_SYSTEM_PROMPT_FILE=/workspace/.role-prompt" \
    "${GC_AGENT_IMAGE}" \
    wrapix-agent run

  # Monitor worker exit in background — collect metadata, run gate, post-gate.
  # FDs redirected to log file to avoid holding gc's pipes open (WaitDelay).
  # This is the live worker→judge pipeline (wx-7ttop):
  #   worker-collect (set commit_range) → gate.sh (nudge judge, poll verdict)
  #   → post-gate.sh (close bead, deploy bead, notifications).
  local monitor_log="${host_log_dir}/monitor.log"
  (
    # best-effort: container may already be gone (killed externally)
    podman wait "$name" || true

    if ! GC_BEAD_ID="${bead_id}" GC_WORKSPACE="${GC_WORKSPACE}" \
      bash "${script_dir}/worker-collect.sh"; then
      echo "monitor: ERROR: worker-collect.sh failed for bead ${bead_id}" >&2
      # Continue — gate.sh will detect missing commit_range and exit 1
    fi

    # Gate check → post-gate pipeline. gate.sh nudges the judge,
    # polls for review_verdict, and exits 0 (approve) or 1 (reject/timeout).
    local gate_exit=0
    GC_BEAD_ID="${bead_id}" \
      bash "${script_dir}/gate.sh" || gate_exit=$?

    local post_gate_reason="approved"
    if [[ "$gate_exit" -ne 0 ]]; then
      post_gate_reason="max_rounds_exceeded"
    fi
    if ! GC_BEAD_ID="${bead_id}" GC_TERMINAL_REASON="$post_gate_reason" \
      GC_WORKSPACE="${GC_WORKSPACE}" GC_CITY_NAME="${GC_CITY_NAME:-}" \
        bash "${script_dir}/post-gate.sh"; then
      echo "monitor: ERROR: post-gate.sh failed for bead ${bead_id} (reason: ${post_gate_reason})" >&2
    fi

    # Mark the worker as fully done so is-running can stop lying (wx-92md7).
    # Written unconditionally — even a failed post-gate means the worker's
    # pipeline is finished, no more work happens here.
    : > "$done_marker"

    # Close the session bead so the pool slot drains (wx-de4cn). Without
    # this, gc's legacy sleep policy suspends the session instead of
    # closing it; with max_active_sessions=1 that blocks future dispatch
    # indefinitely. Closes only the session bead (issue_type=session);
    # the work bead's status is owned by post-gate.sh above.
    if ! gc session close "$SESSION"; then
      echo "monitor: warning: gc session close failed for $SESSION" >&2
    fi
  ) </dev/null >> "$monitor_log" 2>&1 &
}

# ---------------------------------------------------------------------------
# Method dispatch
# ---------------------------------------------------------------------------

case "$METHOD" in

  start)
    check_env
    # Extract agent_template and issue from gc's start JSON and export for
    # sub-calls. agent_template is used by is_worker() fallback detection
    # (wx-aqe4z); issue carries the bead ID from worker formulas (wx-fsqcz).
    if [[ -n "${STDIN_DATA:-}" ]]; then
      GC_AGENT_TEMPLATE="$(echo "$STDIN_DATA" | grep -o '"agent_template" *: *"[^"]*"' | head -1 | grep -o '"[^"]*"$' | tr -d '"')" || GC_AGENT_TEMPLATE=""
      export GC_AGENT_TEMPLATE
      _gc_issue="$(echo "$STDIN_DATA" | grep -o '"issue" *: *"[^"]*"' | head -1 | grep -o '"[^"]*"$' | tr -d '"')" || _gc_issue=""
      if [[ -n "$_gc_issue" ]]; then
        export GC_BEAD_ID="$_gc_issue"
      fi
    fi
    # Infer agent_template from session name before the slow bd query.
    # Named agents (scout, judge, mayor) don't need a dolt round-trip.
    if [[ -z "${GC_AGENT_TEMPLATE:-}" ]]; then
      if is_scout; then GC_AGENT_TEMPLATE="scout"
      elif is_judge; then GC_AGENT_TEMPLATE="judge"
      elif is_mayor; then GC_AGENT_TEMPLATE="mayor"
      fi
      export GC_AGENT_TEMPLATE
    fi
    # Fallback: query bead metadata for agent_template when gc's start JSON
    # omits it — reconciler writes agent_template before calling start. (wx-pq03c)
    if [[ -z "${GC_AGENT_TEMPLATE:-}" ]]; then
      _bead_to_check="${GC_BEAD_ID:-${SESSION}}"
      GC_AGENT_TEMPLATE="$(cd "${GC_WORKSPACE}" && bd show "${_bead_to_check}" --json 2>/dev/null \
        | jq -r '.[0].metadata.agent_template // empty' 2>/dev/null)" || GC_AGENT_TEMPLATE=""
      export GC_AGENT_TEMPLATE
    fi
    if is_worker; then
      worker_start
    else
      persistent_start
    fi
    ;;

  stop)
    _gc_role="$(role_name 2>/dev/null)" || _gc_role=""
    if [[ -n "$_gc_role" ]]; then
      rm -f "$(tmux_sock "$_gc_role")" 2>/dev/null || true
    fi
    name="$(container_name)"
    podman stop "$name" 2>/dev/null || true
    # Workers: preserve stopped container for log inspection (wx-4041e).
    # Pre-start guard (wx-tvj7o) cleans up before creating new containers.
    if ! is_worker; then
      podman rm -f "$name" 2>/dev/null || true
    fi
    ;;

  interrupt)
    if is_worker; then
      : # no-op for workers
    else
      persistent_exec tmux send-keys -t "$(role_name)" C-c
    fi
    ;;

  is-running)
    name="$(container_name)"
    running="$(podman inspect --format '{{.State.Running}}' "$name" 2>/dev/null || echo "false")"
    if [[ "$running" != "true" ]]; then
      if is_worker; then
        # Workers are ephemeral — exit 0 means the task completed, not a
        # crash. Report "true" so gc doesn't treat it as "died during
        # startup" and retry in a loop. The background monitor handles
        # post-completion (collect, gate, post-gate, bead close).
        #
        # Stop lying once the monitor's .done marker appears so gc's pool
        # reconciler can drain this slot (wx-92md7). Marker missing + exit=0
        # means monitor is still running post-gate; keep the "true" lie so
        # the container isn't treated as a startup crash.
        _gc_exit="$(podman inspect --format '{{.State.ExitCode}}' "$name" 2>/dev/null)" || _gc_exit=""
        if [[ "$_gc_exit" == "0" ]]; then
          # ##*worker- strips any prefix (bare 'worker-<bead>' or
          # qualified '<city>-worker-<bead>') down to the bead id.
          _gc_bead="${SESSION##*worker-}"
          if [[ -n "$_gc_bead" && "$_gc_bead" != "$SESSION" && \
                -f "${GC_WORKSPACE:-}/.wrapix/state/worker-${_gc_bead}.done" ]]; then
            running="false"
          else
            running="true"
          fi
        fi
      else
        _gc_sock="$(tmux_sock "$(role_name)")"
        if [[ -S "$_gc_sock" ]] && tmux_via "$(role_name)" list-sessions &>/dev/null; then
          running="true"
        fi
      fi
    fi
    echo "$running"
    ;;

  attach)
    if is_worker; then
      : # no-op
    else
      _gc_role="$(role_name)"
      _gc_sock="$(tmux_sock "$_gc_role")"
      if [[ -S "$_gc_sock" ]] && [[ "$(uname)" != "Darwin" ]]; then
        tmux -S "$_gc_sock" attach -t "$_gc_role"
      else
        # attach needs -it for interactive TTY; can't use tmux_via
        podman exec -it "$(container_name)" tmux -S "/workspace/.wrapix/tmux/${_gc_role}.sock" attach -t "$_gc_role"
      fi
      # Restore terminal state after detach (cursor, alternate screen)
      printf '\033[?25h\033[?1049l' 2>/dev/null
      stty sane 2>/dev/null || true
    fi
    ;;

  peek)
    name="$(container_name)"
    if is_worker; then
      podman logs --tail "${1:-50}" "$name" 2>&1
    else
      persistent_exec tmux capture-pane -t "$(role_name)" -p
    fi
    ;;

  send-keys)
    if is_worker; then
      : # no-op
    else
      persistent_exec tmux send-keys -t "$(role_name)" "$@"
    fi
    ;;

  nudge)
    if is_worker; then
      : # no-op
    else
      name="$(container_name)"
      tmux_target="$(role_name)"
      # Wait for idle (no recent activity in last 2 seconds), then send keys
      _gc_last=0
      _gc_now=0
      for _ in $(seq 1 "${GC_NUDGE_IDLE_TIMEOUT:-30}"); do
        _gc_last="$(persistent_exec tmux display-message -t "$tmux_target" -p '#{pane_last_activity}' 2>/dev/null || echo "0")"
        _gc_now="$(date +%s)"
        if [[ $((_gc_now - _gc_last)) -ge 2 ]]; then
          break
        fi
        sleep 1
      done
      # gc sends nudge message on stdin; send it as tmux keys
      if [[ -n "$STDIN_DATA" ]]; then
        persistent_exec tmux send-keys -t "$tmux_target" "$STDIN_DATA" Enter
      elif [[ $# -gt 0 ]]; then
        persistent_exec tmux send-keys -t "$tmux_target" "$@"
      fi
    fi
    ;;

  get-last-activity)
    if is_worker; then
      echo ""
    else
      # gc expects RFC3339 or empty; tmux returns Unix epoch
      _gc_epoch="$(persistent_exec tmux display-message -t "$(role_name)" -p '#{pane_last_activity}' 2>/dev/null)" || _gc_epoch=""
      if [[ -n "$_gc_epoch" && "$_gc_epoch" != "0" ]]; then
        date -u -d "@${_gc_epoch}" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || echo ""
      else
        echo ""
      fi
    fi
    ;;

  clear-scrollback)
    if is_worker; then
      : # no-op
    else
      persistent_exec tmux clear-history -t "$(role_name)"
    fi
    ;;

  is-attached)
    echo "false"
    ;;

  list-running)
    podman ps --filter "label=gc-city=${GC_CITY_NAME}" --format '{{.Names}}' 2>/dev/null
    ;;

  set-meta)
    name="$(container_name)"
    key="${1:?set-meta requires key}"
    # gc sends value on stdin; fall back to positional arg for direct calls
    value="${STDIN_DATA:-${2:-}}"
    # Store metadata as a file inside the container
    if ! podman exec "$name" sh -c "mkdir -p /tmp/gc-meta && echo '${value}' > /tmp/gc-meta/${key}"; then
      echo "provider: ERROR: set-meta ${key} failed for container ${name}" >&2
      exit 1
    fi
    ;;

  get-meta)
    name="$(container_name)"
    key="${1:?get-meta requires key}"
    podman exec "$name" cat "/tmp/gc-meta/${key}" 2>/dev/null || echo ""
    ;;

  remove-meta)
    name="$(container_name)"
    key="${1:?remove-meta requires key}"
    podman exec "$name" rm -f "/tmp/gc-meta/${key}" 2>/dev/null || true
    ;;

  copy-to)
    name="$(container_name)"
    src="${1:?copy-to requires source path}"
    dst="${2:?copy-to requires destination path}"
    podman cp "$src" "${name}:${dst}"
    ;;

  process-alive)
    name="$(container_name)"
    # gc sends process names on stdin (one per line); fall back to positional arg
    _gc_proc_names="${STDIN_DATA:-${1:-}}"
    if [[ -n "$_gc_proc_names" ]]; then
      # Check each process name — return true if any is alive
      while IFS= read -r pname; do
        [[ -z "$pname" ]] && continue
        if podman exec "$name" pgrep -x "$pname" >/dev/null 2>&1; then
          echo "true"
          exit 0
        fi
      done <<< "$_gc_proc_names"
      echo "false"
    else
      # No process names — check if the container itself is running
      podman inspect --format '{{.State.Running}}' "$name" 2>/dev/null || echo "false"
    fi
    ;;

  check-image)
    image="${1:?check-image requires image name}"
    podman image exists "$image" 2>/dev/null && echo "true" || echo "false"
    ;;

  run-live)
    : # unsupported by exec provider — no-op
    ;;

  capabilities)
    echo "{}"
    ;;

  check-env)
    check_env
    ;;

  *)
    # Exit 2 = unknown operation (forward-compatible no-op per gc exec protocol)
    exit 2
    ;;
esac
