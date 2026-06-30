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

WRIX_FIREWALL_BACKEND="${WRIX_FIREWALL_BACKEND:-}"
WRIX_NFT_BIN=""
WRIX_IPTABLES_BIN=""
WRIX_IP6TABLES_BIN=""
case "$WRIX_FIREWALL_BACKEND" in
  "")
    if WRIX_NFT_BIN="$(command -v nft)"; then
      WRIX_FIREWALL_BACKEND="nft"
    else
      WRIX_NFT_BIN=""
      WRIX_FIREWALL_BACKEND="iptables"
    fi
    ;;
  nft|iptables) ;;
  *)
    echo "Error: WRIX_FIREWALL_BACKEND must be 'nft' or 'iptables' (got: $WRIX_FIREWALL_BACKEND)" >&2
    exit 1
    ;;
esac
if [[ "$WRIX_FIREWALL_BACKEND" = "nft" ]]; then
  if [[ -z "$WRIX_NFT_BIN" ]]; then
    WRIX_NFT_BIN="$(command -v nft)" || { echo "Error: nft is required for sandbox network policy" >&2; exit 1; }
  fi
else
  if [[ -z "$WRIX_IPTABLES_BIN" ]]; then
    WRIX_IPTABLES_BIN="$(command -v iptables)" || { echo "Error: iptables is required for sandbox network policy" >&2; exit 1; }
  fi
  if [[ -z "$WRIX_IP6TABLES_BIN" ]]; then
    WRIX_IP6TABLES_BIN="$(command -v ip6tables)" || { echo "Error: ip6tables is required for sandbox IPv6 blocking" >&2; exit 1; }
  fi
fi
WRIX_CAPSH_BIN="$(command -v capsh)" || { echo "Error: capsh is required to drop NET_ADMIN before agent start" >&2; exit 1; }
WRIX_GETENT_BIN="$(command -v getent)" || { echo "Error: getent is required to resolve network allowlist domains" >&2; exit 1; }
WRIX_AWK_BIN="$(command -v awk)" || { echo "Error: awk is required to resolve network allowlist domains" >&2; exit 1; }
WRIX_SORT_BIN="$(command -v sort)" || { echo "Error: sort is required to resolve network allowlist domains" >&2; exit 1; }
WRIX_GREP_BIN="$(command -v grep)" || { echo "Error: grep is required to verify sandbox network policy" >&2; exit 1; }
export WRIX_FIREWALL_BACKEND WRIX_NFT_BIN WRIX_IPTABLES_BIN WRIX_IP6TABLES_BIN WRIX_CAPSH_BIN WRIX_GETENT_BIN WRIX_AWK_BIN WRIX_SORT_BIN WRIX_GREP_BIN
if WRIX_REAL_BD_BIN="$(command -v bd)"; then
  export WRIX_REAL_BD_BIN
else
  WRIX_REAL_BD_BIN=""
fi

# Prepend /workspace/bin AFTER the entrypoint's own git bootstrap (git-ssh-setup,
# safe.directory, hooksPath) so those resolve the baked git, never a consumer
# shim — but BEFORE the agent guard/dispatch so a /workspace/bin/<agent> shim
# still resolves for the agent.
if [[ -d /workspace/bin ]]; then export PATH="/workspace/bin:$PATH"; fi

wrix_install_bd_remote_wrapper() {
  [[ -n "${WRIX_REAL_BD_BIN:-}" ]] || return 0
  local wrapper_dir="/tmp/wrix-bd"
  mkdir -p "$wrapper_dir"
  cat >"$wrapper_dir/bd" <<'WRIX_BD_WRAPPER'
#!/bin/bash
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
  exec "$real_bd" "$@"
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

# Connect bd to the host workspace service through the launcher-provided
# TCP or Unix-socket endpoint. Missing endpoints are fatal when the Dolt
# backend is configured because embedded autostart would diverge from the
# host's authoritative state.
if [[ -f /workspace/.beads/config.yaml ]]; then
  # best-effort: missing/malformed metadata.json -> default to sqlite backend
  BACKEND=$(jq -r '.backend // "sqlite"' /workspace/.beads/metadata.json 2>/dev/null || echo "sqlite")

  if [[ "$BACKEND" = "dolt" ]]; then
    if [[ -n "${BEADS_DOLT_SERVER_HOST:-}" ]] && [[ -n "${BEADS_DOLT_SERVER_PORT:-}" ]]; then
      export BEADS_DOLT_AUTO_START=0
    elif [[ -n "${BEADS_DOLT_SERVER_SOCKET:-}" ]]; then
      if [[ ! -S "$BEADS_DOLT_SERVER_SOCKET" ]]; then
        echo "Error: configured Dolt socket is unavailable: $BEADS_DOLT_SERVER_SOCKET" >&2
        exit 1
      fi
      export BEADS_DOLT_AUTO_START=0
    elif [[ -S /workspace/.wrix/dolt.sock ]]; then
      export BEADS_DOLT_SERVER_SOCKET=/workspace/.wrix/dolt.sock
      export BEADS_DOLT_AUTO_START=0
    else
      echo "Error: dolt backend configured but no connection available (socket or TCP)" >&2
      _repo=$(git -C /workspace remote get-url origin 2>/dev/null | sed 's|.*/||;s|\.git$||')
      echo "  Start the host ${_repo:-repo}-service container with wrix service start before launching this container." >&2
      exit 1
    fi
    wrix_install_bd_remote_wrapper
  fi

  # best-effort: files may not exist or not be tracked; restoring them is idempotent cleanup
  git checkout -- .beads/.gitignore AGENTS.md 2>/dev/null || true
fi

# BEGIN wrix network policy
wrix_die() {
  echo "Error: $*" >&2
  exit 1
}

wrix_nft() {
  "$WRIX_NFT_BIN" "$@" || wrix_die "nft $* failed"
}

wrix_nft_load_base_ruleset() {
  "$WRIX_NFT_BIN" -f - <<'NFT' || wrix_die "nft base ruleset load failed"
flush ruleset
table inet wrix {
  chain input {
    type filter hook input priority 0; policy drop;
  }

  chain forward {
    type filter hook forward priority 0; policy drop;
  }

  chain output {
    type filter hook output priority 0; policy drop;
  }
}
NFT
}

wrix_iptables() {
  "$WRIX_IPTABLES_BIN" -w "$@" || wrix_die "iptables $* failed"
}

wrix_ip6tables() {
  "$WRIX_IP6TABLES_BIN" -w "$@" || wrix_die "ip6tables $* failed"
}

wrix_firewall_allow_ipv4() {
  local ip="$1"
  local port="$2"
  local proto="$3"
  case "$WRIX_FIREWALL_BACKEND" in
    nft)
      if [[ -n "$port" ]]; then
        wrix_nft add rule inet wrix output ip daddr "$ip" "$proto" dport "$port" accept
      else
        wrix_nft add rule inet wrix output ip daddr "$ip" accept
      fi
      ;;
    iptables)
      if [[ -n "$port" ]]; then
        wrix_iptables -A OUTPUT -p "$proto" -d "$ip" --dport "$port" -j ACCEPT
      else
        wrix_iptables -A OUTPUT -d "$ip" -j ACCEPT
      fi
      ;;
    *) wrix_die "firewall backend was not selected" ;;
  esac
}

wrix_firewall_reject_ipv4_cidr() {
  local cidr="$1"
  case "$WRIX_FIREWALL_BACKEND" in
    nft) wrix_nft add rule inet wrix output ip daddr "$cidr" reject ;;
    iptables) wrix_iptables -A OUTPUT -d "$cidr" -j REJECT ;;
    *) wrix_die "firewall backend was not selected" ;;
  esac
}

wrix_firewall_allow_public_ipv4() {
  case "$WRIX_FIREWALL_BACKEND" in
    nft) wrix_nft add rule inet wrix output meta nfproto ipv4 accept ;;
    iptables) wrix_iptables -A OUTPUT -j ACCEPT ;;
    *) wrix_die "firewall backend was not selected" ;;
  esac
}

wrix_ipv4_is_special() {
  local ip="$1"
  local first second third _fourth
  [[ "$ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || return 1
  IFS=. read -r first second third _fourth <<< "$ip"
  case "$first" in
    0|10|127|224|225|226|227|228|229|230|231|232|233|234|235|236|237|238|239|240|241|242|243|244|245|246|247|248|249|250|251|252|253|254|255)
      return 0
      ;;
    100)
      [[ "$second" -ge 64 && "$second" -le 127 ]]
      ;;
    169)
      [[ "$second" -eq 254 ]]
      ;;
    172)
      [[ "$second" -ge 16 && "$second" -le 31 ]]
      ;;
    192)
      [[ ( "$second" -eq 0 && ( "$third" -eq 0 || "$third" -eq 2 ) ) || ( "$second" -eq 88 && "$third" -eq 99 ) || "$second" -eq 168 ]]
      ;;
    198)
      [[ "$second" -eq 18 || "$second" -eq 19 || ( "$second" -eq 51 && "$third" -eq 100 ) ]]
      ;;
    203)
      [[ "$second" -eq 0 && "$third" -eq 113 ]]
      ;;
    *)
      return 1
      ;;
  esac
}

wrix_resolve_ipv4() {
  local host="$1"
  local records
  if [[ "$host" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf '%s\n' "$host"
    return 0
  fi
  if ! records=$("$WRIX_GETENT_BIN" ahostsv4 "$host" | "$WRIX_AWK_BIN" "{print \$1}" | "$WRIX_SORT_BIN" -u); then
    return 1
  fi
  [[ -n "$records" ]] || return 1
  printf '%s\n' "$records"
}

wrix_allow_ipv4_host() {
  local host="$1"
  local port="$2"
  local proto="$3"
  local reason="$4"
  local records ip
  records=$(wrix_resolve_ipv4 "$host") || wrix_die "$reason host is unresolvable: $host"
  while IFS= read -r ip; do
    [[ -n "$ip" ]] || continue
    wrix_firewall_allow_ipv4 "$ip" "$port" "$proto"
  done <<< "$records"
}

wrix_allow_dns_exceptions() {
  local servers="${WRIX_NETWORK_DNS_SERVERS:-}"
  local kind server _rest
  local -a dns_servers
  if [[ -f /etc/resolv.conf ]]; then
    while read -r kind server _rest; do
      [[ "$kind" = "nameserver" && -n "${server:-}" ]] || continue
      servers="$servers,$server"
    done < /etc/resolv.conf
  fi
  IFS=',' read -ra dns_servers <<< "$servers"
  for server in "${dns_servers[@]}"; do
    [[ "$server" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || continue
    wrix_firewall_allow_ipv4 "$server" 53 udp
    wrix_firewall_allow_ipv4 "$server" 53 tcp
  done
}

wrix_allow_local_endpoints() {
  local endpoints="${WRIX_NETWORK_LOCAL_ENDPOINTS:-}"
  local endpoint target proto host port
  local -a endpoint_entries
  if [[ -n "${BEADS_DOLT_SERVER_HOST:-}" && -n "${BEADS_DOLT_SERVER_PORT:-}" ]]; then
    endpoints="$endpoints,${BEADS_DOLT_SERVER_HOST}:${BEADS_DOLT_SERVER_PORT}/tcp"
  fi
  if [[ -n "${WRIX_NIX_CACHE_HOST:-}" && -n "${WRIX_NIX_CACHE_PORT:-}" ]]; then
    endpoints="$endpoints,${WRIX_NIX_CACHE_HOST}:${WRIX_NIX_CACHE_PORT}/tcp"
  fi
  if [[ -n "${WRIX_PROJECT_CACHE_HOST:-}" && -n "${WRIX_PROJECT_CACHE_PORT:-}" ]]; then
    endpoints="$endpoints,${WRIX_PROJECT_CACHE_HOST}:${WRIX_PROJECT_CACHE_PORT}/tcp"
  fi
  IFS=',' read -ra endpoint_entries <<< "$endpoints"
  for endpoint in "${endpoint_entries[@]}"; do
    [[ -n "$endpoint" ]] || continue
    proto="tcp"
    target="$endpoint"
    if [[ "$endpoint" = */* ]]; then
      proto="${endpoint##*/}"
      target="${endpoint%/*}"
    fi
    [[ "$proto" = "tcp" || "$proto" = "udp" ]] || wrix_die "invalid local endpoint protocol: $endpoint"
    [[ "$target" = *:* ]] || wrix_die "invalid local endpoint, expected host:port: $endpoint"
    host="${target%:*}"
    port="${target##*:}"
    [[ "$port" =~ ^[0-9]+$ ]] || wrix_die "invalid local endpoint port: $endpoint"
    wrix_allow_ipv4_host "$host" "$port" "$proto" "local endpoint"
  done
}

wrix_block_special_ipv4_ranges() {
  local cidr
  local blocked_cidrs=(
    0.0.0.0/8
    10.0.0.0/8
    100.64.0.0/10
    127.0.0.0/8
    169.254.0.0/16
    172.16.0.0/12
    192.0.0.0/24
    192.0.2.0/24
    192.88.99.0/24
    192.168.0.0/16
    198.18.0.0/15
    198.51.100.0/24
    203.0.113.0/24
    224.0.0.0/4
    240.0.0.0/4
  )
  for cidr in "${blocked_cidrs[@]}"; do
    wrix_firewall_reject_ipv4_cidr "$cidr"
  done
}

wrix_allow_limit_domains() {
  local domain records ip
  local -a allowlist_domains
  IFS=',' read -ra allowlist_domains <<< "${WRIX_NETWORK_ALLOWLIST:-}"
  for domain in "${allowlist_domains[@]}"; do
    [[ -n "$domain" ]] || continue
    records=$(wrix_resolve_ipv4 "$domain") || wrix_die "allowlist domain is unresolvable: $domain"
    while IFS= read -r ip; do
      [[ -n "$ip" ]] || continue
      if wrix_ipv4_is_special "$ip"; then
        wrix_die "allowlist domain resolves to blocked local/special address: $domain -> $ip"
      fi
      wrix_firewall_allow_ipv4 "$ip" "" ""
    done <<< "$records"
  done
}

wrix_verify_nft_policy() {
  local input_chain output_chain
  input_chain=$("$WRIX_NFT_BIN" list chain inet wrix input) || wrix_die "could not inspect nft INPUT policy"
  output_chain=$("$WRIX_NFT_BIN" list chain inet wrix output) || wrix_die "could not inspect nft OUTPUT policy"
  "$WRIX_GREP_BIN" -q 'policy drop' <<<"$input_chain" || wrix_die "INPUT default-drop policy was not installed"
  "$WRIX_GREP_BIN" -q 'policy drop' <<<"$output_chain" || wrix_die "OUTPUT default-drop policy was not installed"
  "$WRIX_GREP_BIN" -q 'ip daddr 10.0.0.0/8 reject' <<<"$output_chain" || wrix_die "local-network reject rules were not installed"
}

wrix_verify_iptables_policy() {
  local input_rules output_rules ip6_output_rules
  input_rules=$("$WRIX_IPTABLES_BIN" -S INPUT) || wrix_die "could not inspect iptables INPUT policy"
  output_rules=$("$WRIX_IPTABLES_BIN" -S OUTPUT) || wrix_die "could not inspect iptables OUTPUT policy"
  ip6_output_rules=$("$WRIX_IP6TABLES_BIN" -S OUTPUT) || wrix_die "could not inspect ip6tables OUTPUT policy"
  "$WRIX_GREP_BIN" -qx -- "-P INPUT DROP" <<<"$input_rules" || wrix_die "INPUT default-drop policy was not installed"
  "$WRIX_GREP_BIN" -qx -- "-P OUTPUT DROP" <<<"$output_rules" || wrix_die "OUTPUT default-drop policy was not installed"
  "$WRIX_IPTABLES_BIN" -C OUTPUT -d 10.0.0.0/8 -j REJECT || wrix_die "local-network reject rules were not installed"
  "$WRIX_GREP_BIN" -qx -- "-P OUTPUT DROP" <<<"$ip6_output_rules" || wrix_die "IPv6 OUTPUT default-drop policy was not installed"
}

wrix_verify_firewall_policy() {
  case "$WRIX_FIREWALL_BACKEND" in
    nft) wrix_verify_nft_policy ;;
    iptables) wrix_verify_iptables_policy ;;
    *) wrix_die "firewall backend was not selected" ;;
  esac
}

wrix_reset_firewall_policy() {
  case "$WRIX_FIREWALL_BACKEND" in
    nft)
      wrix_nft_load_base_ruleset
      wrix_nft add rule inet wrix input iifname "lo" accept
      wrix_nft add rule inet wrix input ct state established,related accept
      wrix_nft add rule inet wrix output oifname "lo" accept
      wrix_nft add rule inet wrix output ct state established,related accept
      ;;
    iptables)
      wrix_iptables -F INPUT
      wrix_iptables -F FORWARD
      wrix_iptables -F OUTPUT
      wrix_iptables -P INPUT DROP
      wrix_iptables -P FORWARD DROP
      wrix_iptables -P OUTPUT DROP
      wrix_iptables -A INPUT -i lo -j ACCEPT
      wrix_iptables -A INPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
      wrix_iptables -A OUTPUT -o lo -j ACCEPT
      wrix_iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT

      wrix_ip6tables -F INPUT
      wrix_ip6tables -F FORWARD
      wrix_ip6tables -F OUTPUT
      wrix_ip6tables -P INPUT DROP
      wrix_ip6tables -P FORWARD DROP
      wrix_ip6tables -P OUTPUT DROP
      ;;
    *) wrix_die "firewall backend was not selected" ;;
  esac
}

apply_wrix_network_policy() {
  local mode="${WRIX_NETWORK:-open}"
  case "$mode" in
    open|limit) ;;
    *) wrix_die "WRIX_NETWORK must be 'open' or 'limit' (got: $mode)" ;;
  esac
  [[ -n "${WRIX_FIREWALL_BACKEND:-}" ]] || wrix_die "firewall backend was not selected"

  echo "Network mode: $mode (local-network baseline enforced; firewall=$WRIX_FIREWALL_BACKEND)" >&2
  wrix_reset_firewall_policy
  wrix_allow_dns_exceptions
  wrix_allow_local_endpoints
  wrix_block_special_ipv4_ranges

  if [[ "$mode" = "limit" ]]; then
    wrix_allow_limit_domains
  else
    wrix_firewall_allow_public_ipv4
  fi

  wrix_verify_firewall_policy
}

wrix_verify_net_admin_drop() {
  local probe
  case "$WRIX_FIREWALL_BACKEND" in
    nft) probe="\"\$WRIX_NFT_BIN\" add rule inet wrix output accept >/dev/null 2>&1" ;;
    iptables) probe="\"\$WRIX_IPTABLES_BIN\" -A OUTPUT -j ACCEPT >/dev/null 2>&1" ;;
    *) wrix_die "firewall backend was not selected" ;;
  esac
  if "$WRIX_CAPSH_BIN" --drop=cap_net_admin -- -c "$probe"; then
    wrix_die "NET_ADMIN capability drop could not be verified"
  fi
}

run_without_net_admin() {
  local wrix_entrypoint_path="$PATH"
  wrix_verify_net_admin_drop
  # shellcheck disable=SC2016
  "$WRIX_CAPSH_BIN" --drop=cap_net_admin -- -c 'PATH="$1"; shift; export PATH; exec "$@"' wrix-no-net-admin "$wrix_entrypoint_path" "$@"
}
# END wrix network policy

apply_wrix_network_policy

wrix_session_dir_for_agent() {
  case "${WRIX_AGENT:-direct}" in
    claude) printf '%s\n' "/workspace/.claude" ;;
    pi) printf '%s\n' "/workspace/.pi/agent/sessions" ;;
    direct) printf '%s\n' "/workspace" ;;
    *) printf '%s\n' "/workspace" ;;
  esac
}

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

  local bead_id=""
  if [[ -r /tmp/wrix-bead-id ]]; then
    bead_id=$(cat /tmp/wrix-bead-id)
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
  local log_file="/workspace/.wrix/log/${SESSION_START_ISO//[:.]/-}.json"

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
    }' > "$log_file"
}

# Run main process (without exec, so EXIT trap can write session log)
MAIN_EXIT=0
if [[ $# -gt 0 ]]; then
  # Command override: run the specified command instead of the selected agent.
  run_without_net_admin "$@" || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "pi" ]] && [[ "${WRIX_STDIO:-}" = "1" ]]; then
  # Pi RPC mode: pi listens on stdin/stdout for JSONL commands.
  # Loom drives the session from the host via piped stdio.
  run_without_net_admin pi --mode rpc || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "pi" ]]; then
  run_without_net_admin pi || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "direct" ]]; then
  # Direct mode: loom-direct-runner listens on stdin/stdout for JSONL
  # commands and drives a loom-llm Conversation with the six sandbox-aware
  # tools. Loom drives the session from the host via piped stdio.
  run_without_net_admin loom-direct-runner || MAIN_EXIT=$?
elif [[ "$WRIX_AGENT" = "claude" ]] && [[ "${WRIX_STDIO:-}" = "1" ]]; then
  # Claude stream-json mode: loom drives the session from the host via piped
  # stdio. Symmetric to the pi branch above. Canonical claude args live here
  # (single source of truth) so workflow code doesn't have to thread them.
  run_without_net_admin claude \
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
  run_without_net_admin claude --dangerously-skip-permissions --append-system-prompt "$SYSTEM_PROMPT" || MAIN_EXIT=$?
fi

write_session_log "$MAIN_EXIT"
exit "$MAIN_EXIT"
