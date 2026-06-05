#!/bin/bash
set -euo pipefail

# Record session start for audit trail
SESSION_START_EPOCH=$(date +%s)
SESSION_START_ISO=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# The launcher always passes HOME=/home/wrix; default it so a bare
# `podman run` of this entrypoint (tests, ad-hoc) still has a writable home —
# git config below and the agent config seeding both need $HOME.
export HOME="${HOME:-/home/wrix}"

cd /workspace

# shellcheck source=/dev/null
. /git-ssh-setup.sh

# The process runs as rootless container-root mapped to the invoking host user
# (default boundary) or host-user->root (krun). Either way paths under
# /workspace and nix's libgit2 caches ($XDG_CACHE_HOME) can present an owner
# git/libgit2 reject as "dubious ownership" / "not owned by current user"
# (especially on krun, where libfakeuid makes tools see uid 1000). Trust all
# paths: single identity, effectively root, ephemeral container. Covers git ops
# and nix's git fetcher alike.
git config --global --replace-all safe.directory '*'

# Point core.hooksPath at the prek bundle baked into the image, per
# specs/pre-commit.md § Bead-Container Hook Installation.
if [[ -d /workspace/.git ]] \
  && [[ -f /workspace/.pre-commit-config.yaml ]] \
  && [[ -n "${WRIX_PREK_HOOKS:-}" ]]; then
  if _wrix_hooks_current=$(git -C /workspace config --local --get core.hooksPath); then
    if [[ "$_wrix_hooks_current" != "$WRIX_PREK_HOOKS" ]]; then
      echo "wrix: overriding stale core.hooksPath ($_wrix_hooks_current) -> $WRIX_PREK_HOOKS" >&2
    fi
  fi
  git -C /workspace config --local core.hooksPath "$WRIX_PREK_HOOKS"
  unset _wrix_hooks_current
fi

# Prepend /workspace/bin AFTER the entrypoint's own git bootstrap (git-ssh-setup,
# safe.directory, hooksPath) so those resolve the baked git, never a consumer
# shim — but BEFORE the agent guard/dispatch so a /workspace/bin/<agent> shim
# still resolves for the agent.
if [[ -d /workspace/bin ]]; then export PATH="/workspace/bin:$PATH"; fi

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

# A command override ($# > 0) execs "$@" instead of the agent, so the
# binary-presence guard applies only to agent-exec runs.
if [[ $# -eq 0 ]] && ! command -v "$WRIX_AGENT_BIN" >/dev/null 2>&1; then
  echo "Error: WRIX_AGENT=$WRIX_AGENT selects '$WRIX_AGENT_BIN', but that binary is not present in this image" >&2
  exit 1
fi

if [[ "$WRIX_AGENT" = "claude" ]]; then
  # Initialize Claude config and settings
  # ~/.claude is a container-local directory (tmpfs, not mounted from host) so that
  # user-level settings.json stays separate from project-level settings.json.
  # Persistent session data (history, projects, etc.) is symlinked from
  # /workspace/.claude which IS on the host via the /workspace bind mount.
  mkdir -p "$HOME/.claude"
  cp /etc/wrix/claude-config.json "$HOME/.claude.json"
  cp /etc/wrix/claude-settings.json "$HOME/.claude/settings.json"
  chmod 644 "$HOME/.claude.json" "$HOME/.claude/settings.json"

  # Runtime MCP server selection
  # Images built with mcpRuntime=true include per-server configs in /etc/wrix/mcp/.
  # WRIX_MCP selects which servers to enable (comma-separated, default: all).
  if [[ -d /etc/wrix/mcp ]]; then
    mcp_enabled="${WRIX_MCP:-all}"
    mcp_servers="{}"

    for config_file in /etc/wrix/mcp/*.json; do
      [[ -f "$config_file" ]] || continue
      server_name=$(basename "$config_file" .json)

      # Filter by WRIX_MCP unless "all"
      if [[ "$mcp_enabled" != "all" ]]; then
        if ! echo ",$mcp_enabled," | grep -qF ",$server_name,"; then
          continue
        fi
      fi

      server_config=$(cat "$config_file")

      # Apply runtime env var overrides
      case "$server_name" in
        tmux)
          if [[ -n "${WRIX_MCP_TMUX_AUDIT:-}" ]]; then
            server_config=$(echo "$server_config" | jq --arg v "$WRIX_MCP_TMUX_AUDIT" '.env.TMUX_DEBUG_AUDIT = $v')
          fi
          if [[ -n "${WRIX_MCP_TMUX_AUDIT_FULL:-}" ]]; then
            server_config=$(echo "$server_config" | jq --arg v "$WRIX_MCP_TMUX_AUDIT_FULL" '.env.TMUX_DEBUG_AUDIT_FULL = $v')
          fi
          ;;
      esac

      mcp_servers=$(echo "$mcp_servers" | jq --arg name "$server_name" --argjson config "$server_config" '.[$name] = $config')
    done

    if [[ "$mcp_servers" != "{}" ]]; then
      jq --argjson servers "$mcp_servers" '.mcpServers = $servers' \
        "$HOME/.claude/settings.json" > "$HOME/.claude/settings.json.tmp"
      mv "$HOME/.claude/settings.json.tmp" "$HOME/.claude/settings.json"
    fi
  fi

  # Seed project-level settings if missing, then sync env vars from image
  mkdir -p /workspace/.claude
  if [[ ! -f /workspace/.claude/settings.json ]]; then
    cp /etc/wrix/claude-settings.json /workspace/.claude/settings.json
  else
    jq -s '.[1].env = (.[1].env // {}) * .[0].env | .[1]' \
      /etc/wrix/claude-settings.json /workspace/.claude/settings.json \
      > /workspace/.claude/settings.json.tmp \
      && mv /workspace/.claude/settings.json.tmp /workspace/.claude/settings.json
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

# Connect bd to the host's wrix-beads dolt container via the mounted
# unix socket. The host starts wrix-beads (lib/beads/default.nix shellHook)
# and bind-mounts /workspace/.wrix/dolt.sock into every wrix container.
# Missing socket is fatal when the dolt backend is configured — we refuse
# to fall back to bd's embedded autostart (which would fork a per-container
# dolt that diverges from the host's authoritative state).
if [[ -f /workspace/.beads/config.yaml ]]; then
  # best-effort: missing/malformed metadata.json -> default to sqlite backend
  BACKEND=$(jq -r '.backend // "sqlite"' /workspace/.beads/metadata.json 2>/dev/null || echo "sqlite")

  if [[ "$BACKEND" = "dolt" ]]; then
    if [[ -n "${BEADS_DOLT_SERVER_HOST:-}" ]] && [[ -n "${BEADS_DOLT_SERVER_PORT:-}" ]]; then
      export BEADS_DOLT_AUTO_START=0
    elif [[ -S /workspace/.wrix/dolt.sock ]]; then
      export BEADS_DOLT_SERVER_SOCKET=/workspace/.wrix/dolt.sock
      export BEADS_DOLT_AUTO_START=0
    else
      echo "Error: dolt backend configured but no connection available (socket or TCP)" >&2
      _repo=$(git -C /workspace remote get-url origin 2>/dev/null | sed 's|.*/||;s|\.git$||')
      echo "  Start the host ${_repo:-repo}-beads container (enter the devShell) before launching this container." >&2
      exit 1
    fi
  fi

  # best-effort: files may not exist or not be tracked; restoring them is idempotent cleanup
  git checkout -- .beads/.gitignore AGENTS.md 2>/dev/null || true
fi

# Apply network filtering when WRIX_NETWORK=limit
# Resolves allowlisted domains to IPs and configures iptables OUTPUT chain
if [[ "${WRIX_NETWORK:-open}" = "limit" ]]; then
  echo "Network mode: limit (restricting outbound to allowlist)" >&2

  if ! command -v iptables >/dev/null 2>&1; then
    echo "Warning: iptables not available, network filtering disabled" >&2
    echo "  WRIX_NETWORK=limit requires NET_ADMIN capability (microVM recommended)" >&2
  elif iptables -P OUTPUT DROP; then
    # Allow loopback traffic
    iptables -A OUTPUT -o lo -j ACCEPT

    # Allow established/related connections (responses to allowed requests)
    iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT

    # Allow DNS (needed to resolve allowlisted domains at runtime)
    iptables -A OUTPUT -p udp --dport 53 -j ACCEPT
    iptables -A OUTPUT -p tcp --dport 53 -j ACCEPT

    # Resolve and allow each domain in the allowlist
    IFS=',' read -ra DOMAINS <<< "${WRIX_NETWORK_ALLOWLIST:-}"
    for domain in "${DOMAINS[@]}"; do
      [[ -z "$domain" ]] && continue
      # Resolve domain to IPv4 addresses
      # best-effort: unresolvable domain -> no iptables rule added, traffic stays blocked
      while IFS=' ' read -r ip _rest; do
        [[ -z "$ip" ]] && continue
        iptables -A OUTPUT -d "$ip" -j ACCEPT
      done < <(getent ahostsv4 "$domain" 2>/dev/null | awk '{print $1}' | sort -u)
    done

    # IPv6: set default drop policy and allow same exceptions
    if command -v ip6tables >/dev/null 2>&1 && ip6tables -P OUTPUT DROP; then
      ip6tables -A OUTPUT -o lo -j ACCEPT
      ip6tables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
      ip6tables -A OUTPUT -p udp --dport 53 -j ACCEPT
      ip6tables -A OUTPUT -p tcp --dport 53 -j ACCEPT

      for domain in "${DOMAINS[@]}"; do
        [[ -z "$domain" ]] && continue
        # best-effort: no IPv6 records -> no ip6tables rule, domain only reachable via IPv4
        while IFS=' ' read -r ip _rest; do
          [[ -z "$ip" ]] && continue
          ip6tables -A OUTPUT -d "$ip" -j ACCEPT
        done < <(getent ahostsv6 "$domain" 2>/dev/null | awk '{print $1}' | sort -u)
      done
    fi

    echo "Network filtering active: ${WRIX_NETWORK_ALLOWLIST:-}" >&2
  else
    echo "Warning: iptables -P OUTPUT DROP failed, network filtering disabled" >&2
    echo "  WRIX_NETWORK=limit requires NET_ADMIN capability (microVM recommended)" >&2
  fi
fi

# Session audit trail: write structured log entry on exit
# Log format documented in specs/security.md § Audit Trail
write_session_log() {
  local exit_code="${1:-0}"
  local end_epoch
  end_epoch=$(date +%s)
  local end_iso
  end_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  local duration=$(( end_epoch - SESSION_START_EPOCH ))

  local mode="interactive"
  if [[ "${LOOM_MODE:-}" = "1" ]]; then
    mode="loom"
  fi

  # Read bead ID if loom wrote one during the session
  local bead_id=""
  if [[ -f /tmp/wrix-bead-id ]]; then
    # best-effort: loom didn't write a bead id -> field stays empty in session log
    bead_id=$(cat /tmp/wrix-bead-id 2>/dev/null || true)
  fi

  # Find most recent claude session ID from history
  local claude_session_id=""
  if [[ -f /workspace/.claude/history.jsonl ]]; then
    # best-effort: missing/malformed history.jsonl -> session id stays empty
    claude_session_id=$(tail -1 /workspace/.claude/history.jsonl 2>/dev/null \
      | jq -r '.sessionId // empty' 2>/dev/null || true)
  fi

  mkdir -p /workspace/.wrix/log
  local log_file="/workspace/.wrix/log/${SESSION_START_ISO//[:.]/-}.json"

  # Build JSON with jq to ensure proper escaping
  jq -n \
    --arg start "$SESSION_START_ISO" \
    --arg end "$end_iso" \
    --argjson duration "$duration" \
    --argjson exit_code "$exit_code" \
    --arg mode "$mode" \
    --arg bead_id "$bead_id" \
    --arg session_id "${WRIX_SESSION_ID:-}" \
    --arg claude_session_id "$claude_session_id" \
    --arg claude_session_dir "/workspace/.claude" \
    '{
      timestamp_start: $start,
      timestamp_end: $end,
      duration_seconds: $duration,
      exit_code: $exit_code,
      mode: $mode,
      bead_id: (if $bead_id == "" then null else $bead_id end),
      wrix_session_id: (if $session_id == "" then null else $session_id end),
      claude_session_id: (if $claude_session_id == "" then null else $claude_session_id end),
      claude_session_dir: $claude_session_dir
    }' > "$log_file" 2>/dev/null || true
    # best-effort: /workspace/.wrix/log unwritable in some profiles -> skip session log rather than fail exit trap
}

# Run main process (without exec, so EXIT trap can write session log)
MAIN_EXIT=0
if [[ $# -gt 0 ]]; then
  # Command override: run the specified command instead of the selected agent.
  "$@" || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "pi" ]] && [[ "${WRIX_STDIO:-}" = "1" ]]; then
  # Pi RPC mode: pi listens on stdin/stdout for JSONL commands.
  # Loom drives the session from the host via piped stdio.
  pi --mode rpc || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "pi" ]]; then
  pi || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "direct" ]]; then
  # Direct mode: loom-direct-runner listens on stdin/stdout for JSONL
  # commands and drives a loom-llm Conversation with the six sandbox-aware
  # tools. Loom drives the session from the host via piped stdio.
  loom-direct-runner || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "claude" ]] && [[ "${WRIX_STDIO:-}" = "1" ]]; then
  # Claude stream-json mode: loom drives the session from the host via piped
  # stdio. Symmetric to the pi branch above. Canonical claude args live here
  # (single source of truth) so workflow code doesn't have to thread them.
  claude \
    --dangerously-skip-permissions \
    --print \
    --verbose \
    --input-format stream-json \
    --output-format stream-json \
    || MAIN_EXIT=$?
else
  # Build system prompt only for interactive claude (not needed for command
  # overrides).  Requires /etc/wrix-prompt to be mounted.
  SYSTEM_PROMPT=$(cat /etc/wrix-prompt)
  if [[ -f /workspace/docs/README.md ]]; then
    SYSTEM_PROMPT="$SYSTEM_PROMPT

## Project Context (from docs/README.md)

$(cat /workspace/docs/README.md)"
  fi
  claude --dangerously-skip-permissions --append-system-prompt "$SYSTEM_PROMPT" || MAIN_EXIT=$?
fi

write_session_log "$MAIN_EXIT"
exit "$MAIN_EXIT"
