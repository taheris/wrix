# Notification client
#
# Sends notification requests to the host daemon.
# - Host clients: uses the host-local Unix socket on both platforms
# - Darwin containers: uses TCP to host gateway (VirtioFS can't pass Unix sockets)
# - Linux containers: uses Unix socket (mounted from host)
#
# Succeeds when the best-effort notification daemon is unavailable.
#
# Usage: wrix-notify "Title" "Message" ["Sound"]
{
  pkgs,
  ipCommand ? "${pkgs.iproute2}/bin/ip",
  host ? false,
}:

pkgs.writeShellScriptBin (if host then "wrix-notify-host" else "wrix-notify") ''
  set -euo pipefail

  SOCKET=${
    if host then
      ''"''${XDG_RUNTIME_DIR:-$HOME/.local/share}/wrix/notify.sock"''
    else
      ''"/run/wrix/notify.sock"''
  }
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
    --arg target "''${WRIX_FOCUS_TARGET:-}" \
    '{title: $t, message: $m, sound: $s} +
      (if $target == "" then {} else {focus_target: $target} end)')

  ${pkgs.lib.optionalString (!host) ''
    TCP_PORT=5959

    send_tcp() {
      local tcp_host="$1"
      local tcp_port="$2"

      log_verbose "using TCP to $tcp_host:$tcp_port"
      if ! printf '%s\n' "$payload" | ${pkgs.socat}/bin/socat -u - "TCP:$tcp_host:$tcp_port,connect-timeout=1" >/dev/null 2>/dev/null; then # best-effort: host notification daemon may be absent or disconnect early.
        printf 'wrix-notify: TCP send failed\n' >&2
        return 0
      fi
      log_verbose "sent via TCP"
    }

    resolve_default_gateway() {
      ${ipCommand} route | ${pkgs.gawk}/bin/awk '/default/ {print $3; exit}'
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
        printf 'wrix-notify: invalid TCP endpoint: %s\n' "$tcp_endpoint" >&2
        exit 0
      fi

      send_tcp "$tcp_host" "$tcp_port"
      exit 0
    fi
  ''}

  if [[ ! -S "$SOCKET" ]]; then
    printf 'wrix-notify: socket not found at %s\n' "$SOCKET" >&2
    exit 0
  fi

  if ! printf '%s\n' "$payload" | ${pkgs.socat}/bin/socat -u - "UNIX-CONNECT:$SOCKET" >/dev/null 2>/dev/null; then # best-effort: host notification daemon may be absent or disconnect early.
    printf 'wrix-notify: Unix socket send failed\n' >&2
    exit 0
  fi
  log_verbose "sent to $SOCKET"
  exit 0
''
