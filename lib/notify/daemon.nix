# Host-side notification daemon
#
# Listens for notification requests and triggers native desktop notifications.
#
# On Linux: Listens on Unix socket (mounted into containers)
# On Darwin: Listens on TCP port 5959 (VirtioFS can't pass Unix sockets)
#            Also listens on Unix socket for local testing
#
# Security note (macOS):
#   TCP port 5959 is bound to 192.168.64.1 (vmnet gateway), not 0.0.0.0.
#   Any container on vmnet can send notifications - no authentication.
#   Impact is low: notifications are cosmetic (no code execution).
#   See specs/notifications.md for full security analysis.
#
# Usage: nix run .#wrix-notifyd
{ pkgs, focusBinary }:

let
  inherit (pkgs.stdenv.hostPlatform) isDarwin;
  tcpPort = "5959";
  focus = import ./focus.nix {
    inherit pkgs;
    binary = focusBinary;
  };

  # Platform-specific notification command
  notifyCmd =
    if isDarwin then
      ''
        args=(-title "$title" -message "$msg")
        if [[ -n "$sound" ]]; then args+=(-sound "$sound"); fi
        terminal-notifier "''${args[@]}"
      ''
    else
      ''
        notify-send "$title" "$msg"
      '';

  # Shared handler script with platform-specific parts injected
  handlerScript = ''
    set -euo pipefail

    VERBOSE="''${WRIX_NOTIFY_VERBOSE:-0}"

    check_terminal_focused() {
      local focus_target="$1"
      if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: checking registered origin for $focus_target" >&2; fi
      ${focus}/bin/wrix-focus-notification --is-focused "$focus_target"
    }

    while read -r line; do
      title=$(printf '%s\n' "$line" | jq -r '.title // "Claude Code"')
      msg=$(printf '%s\n' "$line" | jq -r '.message // ""')
      ${if isDarwin then ''sound=$(printf '%s\n' "$line" | jq -r '.sound // ""')'' else ""}
      # Preserve trailing newlines in opaque targets across shell substitution.
      focus_target=$(printf '%s\n' "$line" | jq -r '(.focus_target // "") + "."')
      focus_target="''${focus_target%.}"

      if [[ "''${WRIX_NOTIFY_ALWAYS:-}" != "1" && -n "$focus_target" ]]; then
        if check_terminal_focused "$focus_target"; then
          if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: suppressed (terminal focused)" >&2; fi
          continue
        fi
      fi

      if { ${notifyCmd} }; then
        if [[ -n "$focus_target" ]]; then
          ${focus}/bin/wrix-focus-notification --enqueue "$focus_target" || true # best-effort: queue failure must not prevent notification delivery.
        fi
      else
        echo "notifyd: native notification dispatch failed" >&2
      fi
    done
  '';

in
pkgs.writeShellApplication {
  name = "wrix-notifyd";
  runtimeInputs =
    with pkgs;
    [
      bash
      coreutils
      jq
      socat
    ]
    ++ (if isDarwin then [ terminal-notifier ] else [ libnotify ]);

  text = ''
    SOCKET="''${XDG_RUNTIME_DIR:-$HOME/.local/share}/wrix/notify.sock"
    HANDLER_SCRIPT=""
    SOCKET_PID=""

    cleanup() {
      local status="$?"
      rm -f "$SOCKET"
      if [[ -n "$HANDLER_SCRIPT" ]]; then rm -f "$HANDLER_SCRIPT"; fi
      if [[ -n "$SOCKET_PID" ]]; then
        kill "$SOCKET_PID" 2>/dev/null || true # best-effort: Unix listener may have already exited.
      fi
      return "$status"
    }

    mkdir -p "$(dirname "$SOCKET")"
    rm -f "$SOCKET"
    trap cleanup EXIT

    HANDLER_SCRIPT=$(mktemp)
    cat > "$HANDLER_SCRIPT" << 'HANDLER_EOF'
    ${handlerScript}
    HANDLER_EOF
    chmod +x "$HANDLER_SCRIPT"

    ${
      if isDarwin then
        ''
          echo "wrix-notifyd: listening on TCP port ${tcpPort} and $SOCKET"
          socat UNIX-LISTEN:"$SOCKET",fork EXEC:"bash $HANDLER_SCRIPT" &
          SOCKET_PID="$!"
          socat TCP-LISTEN:${tcpPort},bind=192.168.64.1,fork,reuseaddr EXEC:"bash $HANDLER_SCRIPT"
        ''
      else
        ''
          echo "wrix-notifyd: listening on $SOCKET"
          socat UNIX-LISTEN:"$SOCKET",fork EXEC:"bash $HANDLER_SCRIPT"
        ''
    }
  '';
}
