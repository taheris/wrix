#!/bin/bash
set -euo pipefail

# shellcheck source=/dev/null
. /beads-sandbox.sh

SESSION_START_EPOCH=$(date +%s)
SESSION_START_ISO=$(date -u +%Y-%m-%dT%H:%M:%SZ)
SESSION_LOG_WRITTEN=0

wrix_session_dir_for_agent() {
  case "${WRIX_AGENT:-direct}" in
    claude) printf '%s\n' "/workspace/.claude" ;;
    pi) printf '%s\n' "/workspace/.pi/agent/sessions" ;;
    direct) printf '%s\n' "/workspace" ;;
    *) printf '%s\n' "/workspace" ;;
  esac
}

wrix_bead_id() {
  local spawn_config="${WRIX_SPAWN_CONFIG:-}"
  if [[ -r "$spawn_config" ]]; then
    jq -r '.bead_id | strings | select(length > 0)' "$spawn_config"
  fi
}

write_session_log() {
  local exit_code="$1"
  if [[ "$SESSION_LOG_WRITTEN" -eq 1 ]]; then
    return 0
  fi
  SESSION_LOG_WRITTEN=1

  local end_epoch
  end_epoch=$(date +%s)
  local end_iso
  end_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  local duration=$((end_epoch - SESSION_START_EPOCH))
  local bead_id
  bead_id=$(wrix_bead_id)
  local mode="interactive"
  if [[ -n "$bead_id" || "${LOOM_MODE:-}" = "1" ]]; then
    mode="loom"
  fi

  local claude_session_id=""
  if [[ -f /workspace/.claude/history.jsonl ]]; then
    # best-effort: unreadable or malformed history means the optional id stays empty.
    if ! claude_session_id=$(tail -1 /workspace/.claude/history.jsonl 2>/dev/null \
      | jq -r '.sessionId // empty' 2>/dev/null); then
      claude_session_id=""
    fi
  fi

  local agent_session_dir
  agent_session_dir=$(wrix_session_dir_for_agent)
  mkdir -p "$agent_session_dir" /workspace/.wrix/log
  local log_file
  log_file=$(mktemp --suffix=.json "/workspace/.wrix/log/${SESSION_START_ISO//[:.]/-}.XXXXXX")

  jq -n \
    --arg start "$SESSION_START_ISO" \
    --arg end "$end_iso" \
    --argjson duration "$duration" \
    --argjson exit_code "$exit_code" \
    --arg mode "$mode" \
    --arg bead_id "$bead_id" \
    --arg session_id "${WRIX_SESSION_ID:-}" \
    --arg claude_session_id "$claude_session_id" \
    --arg agent_session_dir "$agent_session_dir" \
    '{
      timestamp_start: $start,
      timestamp_end: $end,
      duration_seconds: $duration,
      exit_code: $exit_code,
      mode: $mode,
      bead_id: (if $bead_id == "" then null else $bead_id end),
      wrix_session_id: (if $session_id == "" then null else $session_id end),
      claude_session_id: (if $claude_session_id == "" then null else $claude_session_id end),
      agent_session_dir: $agent_session_dir
    }' >"$log_file"
}

wrix_on_exit() {
  local exit_code="$?"
  local log_status=0
  trap - EXIT HUP INT TERM
  set +e
  write_session_log "$exit_code"
  log_status=$?
  if [[ "$exit_code" -eq 0 && "$log_status" -ne 0 ]]; then
    exit_code="$log_status"
  fi
  exit "$exit_code"
}

trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
trap wrix_on_exit EXIT

# The immutable network bootstrap installs the firewall, writes this root-owned
# marker, and then replaces itself through capsh. Refuse direct invocation or a
# stage that still carries NET_ADMIN in any capability set.
WRIX_NETWORK_READY_FILE="/run/wrix-network-ready"
if [[ ! -f "$WRIX_NETWORK_READY_FILE" ]]; then
  echo "Error: Darwin network bootstrap did not complete" >&2
  exit 1
fi

wrix_assert_net_admin_absent() {
  local field value low seen=0
  while read -r field value _rest; do
    case "$field" in
      CapInh:|CapPrm:|CapEff:|CapBnd:|CapAmb:)
        [[ "$value" =~ ^[0-9A-Fa-f]+$ ]] || {
          echo "Error: invalid Linux capability state for $field" >&2
          exit 1
        }
        low="${value: -8}"
        if (( (16#$low & 16#1000) != 0 )); then
          echo "Error: NET_ADMIN survived the Darwin network bootstrap ($field)" >&2
          exit 1
        fi
        seen=$((seen + 1))
        ;;
    esac
  done < /proc/self/status
  if [[ "$seen" -ne 5 ]]; then
    echo "Error: Linux capability state could not be verified" >&2
    exit 1
  fi
}

wrix_assert_net_admin_absent
unset WRIX_NETWORK_READY_FILE

# UID mapping strategy for Darwin VirtioFS:
#
# VirtioFS maps all files to UID 0 inside the container. To get correct UID
# matching, we use unshare(1) to create a user namespace at exec time that
# maps inner HOST_UID to outer UID 0. This means:
#   - Setup runs as root (can modify /etc/passwd, create files, set permissions)
#   - All root-owned files automatically appear as HOST_UID inside the namespace
#   - VirtioFS mounts (/workspace) appear as HOST_UID — no ownership mismatch
#   - No chown to HOST_UID needed (counterproductive: outer HOST_UID maps to nobody)
#
# Compare Linux entrypoint which uses Podman's --userns=keep-id for the same effect.

rewrite_wrix_identity() {
  local uid="${HOST_UID:?}"
  local passwd_path="/etc/passwd"
  local group_path="/etc/group"
  local passwd_tmp=""
  local group_tmp=""

  if [[ "$uid" == "0" ]]; then
    return 0
  fi
  if [[ ! -w /etc ]]; then
    echo "Warning: /etc is not writable; skipping wrix UID rewrite" >&2
    return 0
  fi

  passwd_tmp="$(mktemp /etc/passwd.XXXXXX)"
  group_tmp="$(mktemp /etc/group.XXXXXX)"
  sed "s/^wrix:x:[0-9][0-9]*:[0-9][0-9]*:/wrix:x:$uid:$uid:/" "$passwd_path" >"$passwd_tmp"
  sed "s/^wrix:x:[0-9][0-9]*:/wrix:x:$uid:/" "$group_path" >"$group_tmp"
  chmod 0644 "$passwd_tmp" "$group_tmp"
  mv "$passwd_tmp" "$passwd_path"
  mv "$group_tmp" "$group_path"
}

rewrite_wrix_identity

export USER="wrix"
export HOME="/home/wrix"

# Safe path expansion: only expand ~ and $HOME/$USER, not arbitrary commands
expand_path() {
    local p="$1"
    p="${p/#\~/$HOME}"
    p="${p//\$HOME/$HOME}"
    p="${p//\$USER/$USER}"
    echo "$p"
}

# Validate mount mapping format: "src:dst" or "src:dst:ro|rw".
validate_mount_mapping() {
    local mapping="$1"
    [[ "$mapping" =~ ^[^:]+:[^:]+(:ro|:rw)?$ ]]
}

# Copy directories from staging to destination
# VirtioFS maps files as root; unshare namespace remaps root to HOST_UID
# This must run BEFORE SSH setup so deploy keys are in place
if [[ -n "${WRIX_DIR_MOUNTS:-}" ]]; then
    IFS=',' read -ra DIR_MOUNTS <<< "$WRIX_DIR_MOUNTS"
    for mapping in "${DIR_MOUNTS[@]}"; do
        [[ -z "$mapping" ]] && continue
        if ! validate_mount_mapping "$mapping"; then
            echo "Warning: Skipping malformed dir mount: $mapping" >&2
            continue
        fi
        src="${mapping%%:*}"
        mapping_tail="${mapping#*:}"
        if [[ "$mapping_tail" == *:* ]]; then
            mode="${mapping_tail##*:}"
            dst=$(expand_path "${mapping_tail%:*}")
        else
            mode="rw"
            dst=$(expand_path "$mapping_tail")
        fi
        if [[ -d "$src" ]]; then
            mkdir -p "$(dirname "$dst")"
            cp -r "$src" "$dst"
            if [[ "$mode" == "ro" ]]; then
                chmod -R a-w "$dst"
            else
                chmod -R u+w "$dst"
            fi
        fi
    done
fi

# Copy writable files into the guest; read-only files remain on their ro mount.
if [[ -n "${WRIX_FILE_MOUNTS:-}" ]]; then
    IFS=',' read -ra MOUNTS <<< "$WRIX_FILE_MOUNTS"
    for mapping in "${MOUNTS[@]}"; do
        [[ -z "$mapping" ]] && continue
        if ! validate_mount_mapping "$mapping"; then
            echo "Warning: Skipping malformed file mount: $mapping" >&2
            continue
        fi
        src="${mapping%%:*}"
        mapping_tail="${mapping#*:}"
        if [[ "$mapping_tail" == *:* ]]; then
            mode="${mapping_tail##*:}"
            dst=$(expand_path "${mapping_tail%:*}")
        else
            mode="rw"
            dst=$(expand_path "$mapping_tail")
        fi
        if [[ -f "$src" ]]; then
            mkdir -p "$(dirname "$dst")"
            if [[ "$mode" == "ro" ]]; then
                if [[ -d "$dst" && ! -L "$dst" ]]; then
                    echo "Error: read-only file mount destination is a directory: $dst" >&2
                    exit 1
                fi
                rm -f "$dst"
                ln -s "$src" "$dst"
            else
                cp "$src" "$dst"
                chmod u+w "$dst"
            fi
        fi
    done
fi

wrix_sync_file_mounts() {
    local mapping src mapping_tail mode dst
    local -a mounts
    [[ -n "${WRIX_FILE_MOUNTS:-}" ]] || return 0
    IFS=',' read -ra mounts <<< "$WRIX_FILE_MOUNTS"
    for mapping in "${mounts[@]}"; do
        validate_mount_mapping "$mapping" || continue
        src="${mapping%%:*}"
        mapping_tail="${mapping#*:}"
        if [[ "$mapping_tail" == *:* ]]; then
            mode="${mapping_tail##*:}"
            dst=$(expand_path "${mapping_tail%:*}")
        else
            mode="rw"
            dst=$(expand_path "$mapping_tail")
        fi
        [[ "$mode" == "rw" ]] || continue
        if [[ ! -f "$dst" ]]; then
            echo "Error: writable file mount destination disappeared: $dst" >&2
            return 1
        fi
        cp "$dst" "$src"
    done
}

# Copy known_hosts from mounted directory (VirtioFS only supports dirs, not files)
KNOWN_HOSTS_SRC="/etc/wrix/known_hosts_dir/known_hosts"
if [[ -f "$KNOWN_HOSTS_SRC" ]]; then
  cp "$KNOWN_HOSTS_SRC" /etc/ssh/ssh_known_hosts
fi

# Git/SSH setup — shared with Linux entrypoint
# shellcheck source=/dev/null
. /git-ssh-setup.sh

cd /workspace

WRIX_GREP_BIN="$(command -v grep)" || { echo "Error: grep is required by the sandbox entrypoint" >&2; exit 1; }
export WRIX_GREP_BIN
if WRIX_REAL_BD_BIN="$(command -v bd)"; then
  export WRIX_REAL_BD_BIN
else
  WRIX_REAL_BD_BIN=""
fi

if [[ -d /workspace/bin ]]; then export PATH="/workspace/bin:$PATH"; fi

wrix_install_bd_remote_wrapper() {
  [[ -n "${WRIX_REAL_BD_BIN:-}" ]] || return 0
  local wrapper_dir="/tmp/wrix-bd"
  mkdir -p "$wrapper_dir"
  cat >"$wrapper_dir/bd" <<'WRIX_BD_WRAPPER'
#!/usr/bin/env bash
set -euo pipefail

real_bd="${WRIX_REAL_BD_BIN:?}"

wrix_bd_sql_quote() {
  local value="$1"
  value="${value//\'/\'\'}"
  printf "'%s'" "$value"
}

if [[ "${1:-}" != "dolt" || ( "${2:-}" != "pull" && "${2:-}" != "push" ) ]]; then
  exec "$real_bd" "$@"
fi

root="/workspace"
if [[ -f /workspace/.git || -f /workspace/.git/HEAD ]] && git_root=$(git -C /workspace rev-parse --show-toplevel); then
  root="$git_root"
fi
branch="beads"
if [[ -f "$root/.beads/config.yaml" ]] && command -v yq >/dev/null; then
  if parsed_branch=$(yq -r '."sync-branch" // "beads"' "$root/.beads/config.yaml"); then
    if [[ -n "$parsed_branch" && "$parsed_branch" != "null" ]]; then
      branch="$parsed_branch"
    fi
  fi
fi
remote_dir="$root/.git/beads-worktrees/$branch/.beads/dolt-remote"
if [[ ! -d "$remote_dir" ]]; then
  exec "$real_bd" "$@"
fi
desired="file://$remote_dir"
if ! remote_list=$("$real_bd" dolt remote list); then
  echo 'Error: cannot query Dolt remotes; refusing to treat an unavailable database as a missing remote' >&2
  exit 1
fi
original=$(printf '%s\n' "$remote_list" | awk '$1 == "origin" { print $2; exit }')
if [[ "$original" == "$desired" ]]; then
  exec "$real_bd" "$@"
fi

if [[ -n "$original" ]]; then
  "$real_bd" sql "CALL DOLT_REMOTE('remove', 'origin')"
fi
"$real_bd" sql "CALL DOLT_REMOTE('add', 'origin', $(wrix_bd_sql_quote "$desired"))"

wrix_bd_restore_origin() {
  local add_status=0
  local status=0
  set +e
  "$real_bd" sql "CALL DOLT_REMOTE('remove', 'origin')"
  status=$?
  if [[ -n "$original" ]]; then
    "$real_bd" sql "CALL DOLT_REMOTE('add', 'origin', $(wrix_bd_sql_quote "$original"))"
    add_status=$?
    if [[ "$add_status" -ne 0 && "$status" -eq 0 ]]; then
      status=$add_status
    fi
  fi
  set -e
  return "$status"
}

trap 'wrix_bd_restore_origin' EXIT
set +e
"$real_bd" "$@"
command_status=$?
set -e
trap - EXIT
restore_status=0
wrix_bd_restore_origin || restore_status=$?
if [[ "$command_status" -ne 0 ]]; then
  exit "$command_status"
fi
exit "$restore_status"
WRIX_BD_WRAPPER
  chmod +x "$wrapper_dir/bd"
  if [[ "$PATH" == /workspace/bin:* ]]; then
    PATH="/workspace/bin:$wrapper_dir:${PATH#/workspace/bin:}"
  else
    PATH="$wrapper_dir:$PATH"
  fi
  export PATH
}

# Install the image's hook bundle as specified by specs/image-builder.md § Hook Installation.
if [[ -f /workspace/.pre-commit-config.yaml ]] \
  && [[ -n "${WRIX_PREK_HOOKS:-}" ]] \
  && git -C /workspace rev-parse --git-dir >/dev/null 2>&1; then
  if _wrix_hooks_current=$(git -C /workspace config --local --get core.hooksPath); then
    if [[ "$_wrix_hooks_current" != "$WRIX_PREK_HOOKS" ]]; then
      echo "wrix: overriding stale core.hooksPath ($_wrix_hooks_current) -> $WRIX_PREK_HOOKS" >&2
    fi
  fi
  git -C /workspace config --local core.hooksPath "$WRIX_PREK_HOOKS"
  unset _wrix_hooks_current
fi

# WRIX_AGENT selects the agent runtime. 'direct' is the default base image;
# 'claude' and 'pi' are explicit agent overlays. Each agent seeds its own config
# home below (claude ~/.claude, pi ~/.pi/agent); direct has none.
WRIX_AGENT="${WRIX_AGENT:-direct}"
case "$WRIX_AGENT" in
  claude) WRIX_AGENT_BIN=claude ;;
  pi) WRIX_AGENT_BIN=pi ;;
  direct) WRIX_AGENT_BIN=loom-direct-runner ;;
  *)
    echo "Error: unknown WRIX_AGENT: $WRIX_AGENT (expected 'claude', 'pi', or 'direct')" >&2
    exit 1
    ;;
esac

IMAGE_AGENT_FILE="/etc/wrix/image-agent"
if [[ -f "$IMAGE_AGENT_FILE" ]]; then
  IMAGE_AGENT="$(<"$IMAGE_AGENT_FILE")"
else
  IMAGE_AGENT=""
fi
case "$IMAGE_AGENT" in
  ""|claude|pi|direct) ;;
  *)
    echo "Error: image declares unknown agent in $IMAGE_AGENT_FILE: $IMAGE_AGENT (expected 'claude', 'pi', or 'direct')" >&2
    exit 1
    ;;
esac
if [[ -n "$IMAGE_AGENT" && "$IMAGE_AGENT" != "$WRIX_AGENT" ]]; then
  echo "Error: ProfileConfig selected WRIX_AGENT=$WRIX_AGENT, but this image was built for agent=$IMAGE_AGENT; use the matching profile_config for the selected image/agent variant" >&2
  exit 1
fi

# A command override ($# > 0) execs "$@" instead of the agent, so the
# binary-presence guard applies only to agent-exec runs.
if [[ $# -eq 0 ]] && ! command -v "$WRIX_AGENT_BIN" >/dev/null 2>&1; then
  echo "Error: WRIX_AGENT=$WRIX_AGENT selects '$WRIX_AGENT_BIN', but that binary is not present in this image" >&2
  exit 1
fi

# shellcheck source=/dev/null
. /mcp-manifest.sh
wrix_prepare_mcp_manifest

if [[ "$WRIX_AGENT" = "claude" ]]; then
  # Initialize Claude config and settings
  # ~/.claude is a container-local directory (not mounted from host) so that
  # user-level settings.json stays separate from project-level settings.json.
  # Persistent session data (history, projects, etc.) is symlinked from
  # /workspace/.claude which IS on the host via the /workspace VirtioFS mount.
  mkdir -p "$HOME/.claude"
  cp /etc/wrix/claude-config.json "$HOME/.claude.json"
  cp /etc/wrix/claude-settings.json "$HOME/.claude/settings.json"
  chmod 644 "$HOME/.claude.json" "$HOME/.claude/settings.json"

  if [[ -n "${WRIX_MCP_MANIFEST:-}" ]]; then
    mcp_servers=$(jq '
      .servers
      | map({ key: .name, value: { command: .command, args: .args, env: .env } })
      | from_entries
    ' "$WRIX_MCP_MANIFEST")
    if [[ "$mcp_servers" != "{}" ]]; then
      jq --argjson servers "$mcp_servers" '.mcpServers = $servers' \
        "$HOME/.claude.json" > "$HOME/.claude.json.tmp"
      mv "$HOME/.claude.json.tmp" "$HOME/.claude.json"
    fi
  fi

  # Write project-level settings only if missing (preserve user customizations)
  if [[ ! -f /workspace/.claude/settings.json ]]; then
    cp /etc/wrix/claude-settings.json /workspace/.claude/settings.json
  fi

  # Symlink persistent session data from workspace for /resume and /rename
  for item in projects plans todos file-history paste-cache backups \
              debug session-env plugins shell-snapshots \
              history.jsonl settings.local.json stats-cache.json; do
    if [[ -e "/workspace/.claude/$item" ]] && [[ ! -e "$HOME/.claude/$item" ]]; then
      ln -s "/workspace/.claude/$item" "$HOME/.claude/$item"
    fi
  done
elif [[ "$WRIX_AGENT" = "pi" ]]; then
  # Pi keeps its own config home at ~/.pi/agent: seed image-baked defaults
  # when present. Credentials arrive by mount, not seeding (specs/security.md).
  mkdir -p "$HOME/.pi/agent" /workspace/.pi/agent/sessions
  if [[ -d /etc/wrix/pi-agent ]]; then
    cp -rn /etc/wrix/pi-agent/. "$HOME/.pi/agent/"
  fi
  if [[ -n "${WRIX_PI_AUTH_JSON:-}" ]]; then
    if [[ ! -f "$WRIX_PI_AUTH_JSON" ]]; then
      echo "Error: WRIX_PI_AUTH_JSON=$WRIX_PI_AUTH_JSON is not mounted" >&2
      exit 1
    fi
    ln -sf "$WRIX_PI_AUTH_JSON" "$HOME/.pi/agent/auth.json"
  fi
fi

wrix_configure_beads_endpoint /workspace
if [[ "$WRIX_BEADS_CONFIGURED" == "1" ]]; then
  wrix_install_bd_remote_wrapper
fi
wrix_wait_for_beads_endpoint
if [[ -f /workspace/.beads/config.yaml ]]; then

  if [[ -e /workspace/.git ]]; then
    WRIX_TRACKED_BEADS_GITIGNORE="$(git ls-files -- .beads/.gitignore)"
    if [[ -n "$WRIX_TRACKED_BEADS_GITIGNORE" ]]; then
      git checkout -- .beads/.gitignore
    fi
    unset WRIX_TRACKED_BEADS_GITIGNORE
  fi
fi

# Network setup and the NET_ADMIN drop are complete before this stage begins.
# Keep command dispatch uniform with Linux's post-policy call sites.
run_without_net_admin() {
  "$@"
}

# Drop to HOST_UID via user namespace (maps inner HOST_UID to outer root,
# so VirtioFS root-owned files appear as HOST_UID — proper UID mapping)
# Run without exec so session log can be written after exit
MAIN_EXIT=0
if [[ $# -gt 0 ]]; then
  # Command override: run the specified command instead of the selected agent.
  run_without_net_admin unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- \
    "$@" || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "pi" ]] && [[ "${WRIX_STDIO:-}" = "1" ]]; then
  # Pi RPC mode: pi listens on stdin/stdout for JSONL commands.
  # Loom drives the session from the host via piped stdio.
  run_without_net_admin unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- \
    pi --mode rpc || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "pi" ]]; then
  run_without_net_admin unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- \
    pi || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "direct" ]]; then
  # Direct mode: loom-direct-runner listens on stdin/stdout for JSONL
  # commands and drives a loom-llm Conversation with the six sandbox-aware
  # tools. Loom drives the session from the host via piped stdio.
  run_without_net_admin unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- \
    loom-direct-runner || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "claude" ]] && [[ "${WRIX_STDIO:-}" = "1" ]]; then
  # Claude stream-json mode: loom drives the session from the host via piped
  # stdio. Symmetric to the pi branch above. Canonical claude args live here
  # (single source of truth) so workflow code doesn't have to thread them.
  run_without_net_admin unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- \
    claude \
      --dangerously-skip-permissions \
      --print \
      --verbose \
      --input-format stream-json \
      --output-format stream-json \
      || MAIN_EXIT=$?
else
  # Build system prompt only for interactive claude (not needed for command
  # overrides).  Requires /etc/wrix-prompts/wrix-prompt.
  SYSTEM_PROMPT=$(cat /etc/wrix-prompts/wrix-prompt)
  if [[ -f /workspace/docs/README.md ]]; then
    SYSTEM_PROMPT="$SYSTEM_PROMPT

## Project Context (from docs/README.md)

$(cat /workspace/docs/README.md)"
  fi
  run_without_net_admin unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- \
    claude --dangerously-skip-permissions --append-system-prompt "$SYSTEM_PROMPT" || MAIN_EXIT=$?
fi

FILE_SYNC_EXIT=0
wrix_sync_file_mounts || FILE_SYNC_EXIT=$?
if [[ "$MAIN_EXIT" -eq 0 && "$FILE_SYNC_EXIT" -ne 0 ]]; then
  MAIN_EXIT="$FILE_SYNC_EXIT"
fi

exit "$MAIN_EXIT"
