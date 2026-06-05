# Container-side notification client
#
# Sends notification requests to the host daemon.
# - Darwin containers: uses TCP to host gateway (VirtioFS can't pass Unix sockets)
# - Linux containers: uses Unix socket (mounted from host)
#
# Silently succeeds if daemon is not running.
#
# Usage: wrix-notify "Title" "Message" ["Sound"]
{ pkgs }:

pkgs.writeShellScriptBin "wrix-notify" ''
  SOCKET="/run/wrix/notify.sock"
  TCP_PORT=5959      # Must match daemon.nix
  VERBOSE="''${WRIX_NOTIFY_VERBOSE:-0}"
  title="''${1:-Claude Code}"
  message="''${2:-}"
  sound="''${3:-}"

  # Build JSON payload (compact single-line for line-based daemon protocol)
  # Include session_id for focus-aware notifications (empty if not in tmux)
  payload=$(${pkgs.jq}/bin/jq -cn --arg t "$title" --arg m "$message" --arg s "$sound" \
    --arg sid "''${WRIX_SESSION_ID:-}" \
    '{title: $t, message: $m, sound: $s, session_id: $sid}')

  # Darwin containers set WRIX_NOTIFY_TCP=1 (VirtioFS can't pass Unix sockets)
  if [ "''${WRIX_NOTIFY_TCP:-}" = "1" ]; then
    # Get gateway IP (the host) from routing table
    GATEWAY=$(${pkgs.iproute2}/bin/ip route | ${pkgs.gawk}/bin/awk '/default/ {print $3; exit}')
    if [ -z "$GATEWAY" ]; then
      [ "$VERBOSE" = "1" ] && echo "wrix-notify: no gateway found" >&2
      exit 0
    fi
    [ "$VERBOSE" = "1" ] && echo "wrix-notify: using TCP to $GATEWAY:$TCP_PORT" >&2
    printf '%s\n' "$payload" | ${pkgs.netcat}/bin/nc -N "$GATEWAY" "$TCP_PORT" 2>/dev/null || true
    [ "$VERBOSE" = "1" ] && echo "wrix-notify: sent via TCP" >&2
    exit 0
  fi

  # Linux containers: use Unix socket mounted from host
  if [ ! -S "$SOCKET" ]; then
    [ "$VERBOSE" = "1" ] && echo "wrix-notify: socket not found at $SOCKET" >&2
    exit 0  # Silent success if daemon not running
  fi

  printf '%s\n' "$payload" | ${pkgs.netcat}/bin/nc -U -N "$SOCKET" 2>/dev/null || true
  [ "$VERBOSE" = "1" ] && echo "wrix-notify: sent to $SOCKET" >&2
  exit 0
''
