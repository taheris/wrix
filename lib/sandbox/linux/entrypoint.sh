#!/bin/bash
set -euo pipefail

# Record session start for audit trail
SESSION_START_EPOCH=$(date +%s)
SESSION_START_ISO=$(date -u +%Y-%m-%dT%H:%M:%SZ)

cd /workspace

# shellcheck source=/dev/null
. /git-ssh-setup.sh

# Initialize Claude config and settings
# ~/.claude is a container-local directory (tmpfs, not mounted from host) so that
# user-level settings.json stays separate from project-level settings.json.
# Persistent session data (history, projects, etc.) is symlinked from
# /workspace/.claude which IS on the host via the /workspace bind mount.
mkdir -p "$HOME/.claude"
cp /etc/wrapix/claude-config.json "$HOME/.claude.json"
cp /etc/wrapix/claude-settings.json "$HOME/.claude/settings.json"
chmod 644 "$HOME/.claude.json" "$HOME/.claude/settings.json"

# Runtime MCP server selection
# Images built with mcpRuntime=true include per-server configs in /etc/wrapix/mcp/.
# WRAPIX_MCP selects which servers to enable (comma-separated, default: all).
if [ -d /etc/wrapix/mcp ]; then
  mcp_enabled="${WRAPIX_MCP:-all}"
  mcp_servers="{}"

  for config_file in /etc/wrapix/mcp/*.json; do
    [ -f "$config_file" ] || continue
    server_name=$(basename "$config_file" .json)

    # Filter by WRAPIX_MCP unless "all"
    if [ "$mcp_enabled" != "all" ]; then
      if ! echo ",$mcp_enabled," | grep -qF ",$server_name,"; then
        continue
      fi
    fi

    server_config=$(cat "$config_file")

    # Apply runtime env var overrides
    case "$server_name" in
      tmux)
        if [ -n "${WRAPIX_MCP_TMUX_AUDIT:-}" ]; then
          server_config=$(echo "$server_config" | jq --arg v "$WRAPIX_MCP_TMUX_AUDIT" '.env.TMUX_DEBUG_AUDIT = $v')
        fi
        if [ -n "${WRAPIX_MCP_TMUX_AUDIT_FULL:-}" ]; then
          server_config=$(echo "$server_config" | jq --arg v "$WRAPIX_MCP_TMUX_AUDIT_FULL" '.env.TMUX_DEBUG_AUDIT_FULL = $v')
        fi
        ;;
    esac

    mcp_servers=$(echo "$mcp_servers" | jq --arg name "$server_name" --argjson config "$server_config" '.[$name] = $config')
  done

  if [ "$mcp_servers" != "{}" ]; then
    jq --argjson servers "$mcp_servers" '.mcpServers = $servers' \
      "$HOME/.claude/settings.json" > "$HOME/.claude/settings.json.tmp"
    mv "$HOME/.claude/settings.json.tmp" "$HOME/.claude/settings.json"
  fi
fi

# Seed project-level settings if missing, then sync env vars from image
if [ ! -f /workspace/.claude/settings.json ]; then
  cp /etc/wrapix/claude-settings.json /workspace/.claude/settings.json
else
  jq -s '.[1].env = (.[1].env // {}) * .[0].env | .[1]' \
    /etc/wrapix/claude-settings.json /workspace/.claude/settings.json \
    > /workspace/.claude/settings.json.tmp \
    && mv /workspace/.claude/settings.json.tmp /workspace/.claude/settings.json
fi

# === ralph settings merge: start ===
# Deep-merge ralph runtime settings fragment (SessionStart[compact] re-pin hook)
# into ~/.claude/settings.json. Array entries under each hook event are
# concatenated so ralph's hooks coexist with the sandbox Notification hook.
if [ -n "${RALPH_RUNTIME_DIR:-}" ] && [ -f "$RALPH_RUNTIME_DIR/claude-settings.json" ]; then
  jq -s '
    .[0] as $base
    | .[1] as $frag
    | ($base.hooks // {}) as $bh
    | ($frag.hooks // {}) as $fh
    | $base
    | .hooks = (
        ($bh * $fh)
        | with_entries(.value = (($bh[.key] // []) + ($fh[.key] // [])))
      )
  ' "$HOME/.claude/settings.json" "$RALPH_RUNTIME_DIR/claude-settings.json" \
    > "$HOME/.claude/settings.json.tmp"
  mv "$HOME/.claude/settings.json.tmp" "$HOME/.claude/settings.json"
fi
# === ralph settings merge: end ===

# Symlink persistent session data from workspace for /resume and /rename
for item in projects plans todos file-history paste-cache backups \
            debug session-env plugins shell-snapshots \
            history.jsonl settings.local.json stats-cache.json; do
  if [ -e "/workspace/.claude/$item" ] && [ ! -e "$HOME/.claude/$item" ]; then
    ln -s "/workspace/.claude/$item" "$HOME/.claude/$item"
  fi
done

# Connect bd to the host's wrapix-beads dolt container via the mounted
# unix socket. The host starts wrapix-beads (lib/beads/default.nix shellHook)
# and bind-mounts /workspace/.wrapix/dolt.sock into every wrapix container.
# Missing socket is fatal when the dolt backend is configured — we refuse
# to fall back to bd's embedded autostart (which would fork a per-container
# dolt that diverges from the host's authoritative state).
if [ -f /workspace/.beads/config.yaml ]; then
  BACKEND=$(jq -r '.backend // "sqlite"' /workspace/.beads/metadata.json 2>/dev/null || echo "sqlite")

  if [ "$BACKEND" = "dolt" ]; then
    if [ ! -S /workspace/.wrapix/dolt.sock ]; then
      echo "Error: dolt backend configured but /workspace/.wrapix/dolt.sock not mounted" >&2
      _repo=$(git -C /workspace remote get-url origin 2>/dev/null | sed 's|.*/||;s|\.git$||')
      echo "  Start the host ${_repo:-repo}-beads container (enter the devShell) before launching this container." >&2
      exit 1
    fi
    export BEADS_DOLT_SERVER_SOCKET=/workspace/.wrapix/dolt.sock
    export BEADS_DOLT_AUTO_START=0
  else
    PREFIX=$(yq -r '.["issue-prefix"] // ""' /workspace/.beads/config.yaml 2>/dev/null || echo "")
    if [ -n "$PREFIX" ] && [ -f /workspace/.beads/issues.jsonl ]; then
      bd init --prefix "$PREFIX" --from-jsonl --quiet --skip-hooks --skip-merge-driver
    fi
  fi

  git checkout -- .beads/.gitignore AGENTS.md 2>/dev/null || true
fi

# Apply network filtering when WRAPIX_NETWORK=limit
# Resolves allowlisted domains to IPs and configures iptables OUTPUT chain
if [ "${WRAPIX_NETWORK:-open}" = "limit" ]; then
  echo "Network mode: limit (restricting outbound to allowlist)" >&2

  if iptables -P OUTPUT DROP 2>/dev/null; then
    # Allow loopback traffic
    iptables -A OUTPUT -o lo -j ACCEPT

    # Allow established/related connections (responses to allowed requests)
    iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT

    # Allow DNS (needed to resolve allowlisted domains at runtime)
    iptables -A OUTPUT -p udp --dport 53 -j ACCEPT
    iptables -A OUTPUT -p tcp --dport 53 -j ACCEPT

    # Resolve and allow each domain in the allowlist
    IFS=',' read -ra DOMAINS <<< "${WRAPIX_NETWORK_ALLOWLIST:-}"
    for domain in "${DOMAINS[@]}"; do
      [ -z "$domain" ] && continue
      # Resolve domain to IPv4 addresses
      while IFS=' ' read -r ip _rest; do
        [ -z "$ip" ] && continue
        iptables -A OUTPUT -d "$ip" -j ACCEPT
      done < <(getent ahostsv4 "$domain" 2>/dev/null | awk '{print $1}' | sort -u)
    done

    # IPv6: set default drop policy and allow same exceptions
    if ip6tables -P OUTPUT DROP 2>/dev/null; then
      ip6tables -A OUTPUT -o lo -j ACCEPT
      ip6tables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
      ip6tables -A OUTPUT -p udp --dport 53 -j ACCEPT
      ip6tables -A OUTPUT -p tcp --dport 53 -j ACCEPT

      for domain in "${DOMAINS[@]}"; do
        [ -z "$domain" ] && continue
        while IFS=' ' read -r ip _rest; do
          [ -z "$ip" ] && continue
          ip6tables -A OUTPUT -d "$ip" -j ACCEPT
        done < <(getent ahostsv6 "$domain" 2>/dev/null | awk '{print $1}' | sort -u)
      done
    fi

    echo "Network filtering active: ${WRAPIX_NETWORK_ALLOWLIST:-}" >&2
  else
    echo "Warning: iptables not available, network filtering disabled" >&2
    echo "  WRAPIX_NETWORK=limit requires NET_ADMIN capability (microVM recommended)" >&2
  fi
fi

# Session audit trail: write structured log entry on exit
# Log format documented in specs/security-review.md
write_session_log() {
  local exit_code="${1:-0}"
  local end_epoch
  end_epoch=$(date +%s)
  local end_iso
  end_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  local duration=$(( end_epoch - SESSION_START_EPOCH ))

  local mode="interactive"
  if [ "${RALPH_MODE:-}" = "1" ]; then
    mode="ralph"
  fi

  # Read bead ID if ralph wrote one during the session
  local bead_id=""
  if [ -f /tmp/wrapix-bead-id ]; then
    bead_id=$(cat /tmp/wrapix-bead-id 2>/dev/null || true)
  fi

  # Find most recent claude session ID from history
  local claude_session_id=""
  if [ -f /workspace/.claude/history.jsonl ]; then
    claude_session_id=$(tail -1 /workspace/.claude/history.jsonl 2>/dev/null \
      | jq -r '.sessionId // empty' 2>/dev/null || true)
  fi

  mkdir -p /workspace/.wrapix/log
  local log_file="/workspace/.wrapix/log/${SESSION_START_ISO//[:.]/-}.json"

  # Build JSON with jq to ensure proper escaping
  jq -n \
    --arg start "$SESSION_START_ISO" \
    --arg end "$end_iso" \
    --argjson duration "$duration" \
    --argjson exit_code "$exit_code" \
    --arg mode "$mode" \
    --arg bead_id "$bead_id" \
    --arg session_id "${WRAPIX_SESSION_ID:-}" \
    --arg claude_session_id "$claude_session_id" \
    --arg claude_session_dir "/workspace/.claude" \
    '{
      timestamp_start: $start,
      timestamp_end: $end,
      duration_seconds: $duration,
      exit_code: $exit_code,
      mode: $mode,
      bead_id: (if $bead_id == "" then null else $bead_id end),
      wrapix_session_id: (if $session_id == "" then null else $session_id end),
      claude_session_id: (if $claude_session_id == "" then null else $claude_session_id end),
      claude_session_dir: $claude_session_dir
    }' > "$log_file" 2>/dev/null || true
}

# Run main process (without exec, so EXIT trap can write session log)
MAIN_EXIT=0
if [ $# -gt 0 ]; then
  # Command override: run the specified command instead of Claude/Ralph
  "$@" || MAIN_EXIT=$?
elif [ "${RALPH_MODE:-}" = "1" ]; then
  # RALPH_CMD and RALPH_ARGS set by launcher (default: help)
  # shellcheck disable=SC2086 # Intentional word splitting for RALPH_ARGS
  ralph "${RALPH_CMD:-help}" ${RALPH_ARGS:-} || MAIN_EXIT=$?
else
  # Build system prompt only for interactive claude (not needed for command
  # overrides or ralph mode).  Requires /etc/wrapix-prompt to be mounted.
  SYSTEM_PROMPT=$(cat /etc/wrapix-prompt)
  if [ -f /workspace/specs/README.md ]; then
    SYSTEM_PROMPT="$SYSTEM_PROMPT

## Project Context (from specs/README.md)

$(cat /workspace/specs/README.md)"
  fi
  claude --dangerously-skip-permissions --append-system-prompt "$SYSTEM_PROMPT" || MAIN_EXIT=$?
fi

write_session_log "$MAIN_EXIT"
exit "$MAIN_EXIT"
