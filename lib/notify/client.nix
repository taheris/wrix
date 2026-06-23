# Container-side notification client
#
# Sends notification requests to the host daemon.
# - Darwin containers: uses TCP to host gateway (VirtioFS can't pass Unix sockets)
# - Linux containers: uses Unix socket (mounted from host)
#
# Succeeds when the best-effort notification daemon is unavailable.
#
# Usage: wrix-notify "Title" "Message" ["Sound"]
{ pkgs }:

pkgs.writeShellScriptBin "wrix-notify" ''
  set -euo pipefail

  SOCKET="/run/wrix/notify.sock"
  TCP_PORT=5959
  VERBOSE="''${WRIX_NOTIFY_VERBOSE:-0}"
  title="''${1:-Claude Code}"
  message="''${2:-}"
  sound="''${3:-}"

  log_verbose() {
    if [[ "$VERBOSE" == "1" ]]; then
      printf 'wrix-notify: %s\n' "$1" >&2
    fi
  }

  payload=$(${pkgs.jq}/bin/jq -cn --arg t "$title" --arg m "$message" --arg s "$sound" \
    --arg sid "''${WRIX_SESSION_ID:-}" \
    '{title: $t, message: $m, sound: $s, session_id: $sid}')

  send_tcp() {
    local tcp_host="$1"
    local tcp_port="$2"

    log_verbose "using TCP to $tcp_host:$tcp_port"
    if ! printf '%s\n' "$payload" | ${pkgs.socat}/bin/socat -u - "TCP:$tcp_host:$tcp_port,connect-timeout=1" >/dev/null 2>/dev/null; then # best-effort: host notification daemon may be absent or disconnect early.
      log_verbose "TCP send failed"
    fi
    log_verbose "sent via TCP"
  }

  resolve_default_gateway() {
    ${pkgs.iproute2}/bin/ip route | ${pkgs.gawk}/bin/awk '/default/ {print $3; exit}'
  }

  tcp_endpoint="''${WRIX_NOTIFY_TCP:-}"
  if [[ -n "$tcp_endpoint" ]]; then
    if [[ "$tcp_endpoint" == "1" ]]; then
      tcp_host=$(resolve_default_gateway)
      tcp_port="$TCP_PORT"
    else
      tcp_host="''${tcp_endpoint%:*}"
      tcp_port="''${tcp_endpoint##*:}"
    fi

    if [[ -z "$tcp_host" || -z "$tcp_port" || "$tcp_host" == "$tcp_endpoint" || ! "$tcp_port" =~ ^[0-9]+$ ]]; then
      log_verbose "invalid TCP endpoint: $tcp_endpoint"
      exit 0
    fi

    send_tcp "$tcp_host" "$tcp_port"
    exit 0
  fi

  if [[ ! -S "$SOCKET" ]]; then
    log_verbose "socket not found at $SOCKET"
    exit 0
  fi

  if ! printf '%s\n' "$payload" | ${pkgs.socat}/bin/socat -u - "UNIX-CONNECT:$SOCKET" >/dev/null 2>/dev/null; then # best-effort: host notification daemon may be absent or disconnect early.
    log_verbose "Unix socket send failed"
  fi
  log_verbose "sent to $SOCKET"
  exit 0
''
