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
{ pkgs }:

let
  inherit (pkgs.stdenv) isDarwin;
  tcpPort = "5959";

  # Platform-specific session directory
  sessionDir =
    if isDarwin then
      ''WRIX_SESSION_DIR="''${XDG_DATA_HOME:-$HOME/.local/share}/wrix/sessions"''
    else
      ''WRIX_SESSION_DIR="''${XDG_RUNTIME_DIR:-$HOME/.local/share}/wrix/sessions"'';

  # Platform-specific app/window focus check
  # Returns 0 if terminal app/window is focused, 1 otherwise
  appCheckFn =
    if isDarwin then
      ''
        check_app_focused() {
          local session_file="$1"
          local session_app
          session_app=$(jq -r '.terminal_app // ""' "$session_file")
          if [ -z "$session_app" ]; then
            [ "$VERBOSE" = "1" ] && echo "notifyd: no terminal_app in session file" >&2
            return 0  # No app info = assume focused (proceed to tmux check)
          fi

          local focused_app
          if ! focused_app=$(osascript -e 'tell application "System Events" to name of first process whose frontmost is true' 2>/dev/null); then
            [ "$VERBOSE" = "1" ] && echo "notifyd: osascript failed" >&2
            return 0  # Can't check = assume focused
          fi

          [ "$VERBOSE" = "1" ] && echo "notifyd: app session=$session_app focused=$focused_app" >&2
          [ "$focused_app" = "$session_app" ]
        }
      ''
    else
      ''
        check_app_focused() {
          local session_file="$1"
          # Try niri window ID check if available
          if command -v niri &>/dev/null; then
            local session_window_id
            session_window_id=$(jq -r '.window_id // ""' "$session_file")
            if [ -n "$session_window_id" ]; then
              local focused_window_id
              if focused_window_id=$(niri msg -j focused-window 2>/dev/null | jq -r '.id // ""'); then
                [ "$VERBOSE" = "1" ] && echo "notifyd: window session=$session_window_id focused=$focused_window_id" >&2
                [ "$focused_window_id" = "$session_window_id" ]
                return
              fi
            fi
          fi
          # No niri or no window_id = assume app focused (proceed to tmux check)
          [ "$VERBOSE" = "1" ] && echo "notifyd: skipping window check (niri unavailable or no window_id)" >&2
          return 0
        }
      '';

  # Platform-specific notification command
  notifyCmd =
    if isDarwin then
      ''
        args=(-title "$title" -message "$msg")
        [ -n "$sound" ] && args+=(-sound "$sound")
        terminal-notifier "''${args[@]}"
      ''
    else
      ''
        notify-send "$title" "$msg"
      '';

  # Shared handler script with platform-specific parts injected
  handlerScript = ''
    ${sessionDir}
    VERBOSE="''${WRIX_NOTIFY_VERBOSE:-0}"

    ${appCheckFn}

    # Focus check - two levels:
    # 1. App/window level (platform-specific)
    # 2. Tmux pane level (shared)
    check_terminal_focused() {
      local session_id="$1"
      local safe_id="''${session_id//[:\.]/-}"
      local session_file="$WRIX_SESSION_DIR/$safe_id.json"

      if [ ! -f "$session_file" ]; then
        [ "$VERBOSE" = "1" ] && echo "notifyd: session file not found: $session_file" >&2
        return 1  # No session = show notification
      fi

      # Level 1: App/window check (platform-specific)
      if ! check_app_focused "$session_file"; then
        return 1  # Different app/window = show notification
      fi

      # Level 2: Tmux pane check (shared)
      if command -v tmux &>/dev/null && tmux list-sessions &>/dev/null; then
        local active_pane
        active_pane=$(tmux display-message -p '#{session_name}:#{window_index}.#{pane_index}' 2>/dev/null) || true
        [ "$VERBOSE" = "1" ] && echo "notifyd: pane session=$session_id active=$active_pane" >&2
        if [ -n "$active_pane" ] && [ "$active_pane" != "$session_id" ]; then
          return 1  # Different pane = show notification
        fi
      fi

      return 0  # Suppress
    }

    while read -r line; do
      title=$(printf '%s\n' "$line" | jq -r '.title // "Claude Code"')
      msg=$(printf '%s\n' "$line" | jq -r '.message // ""')
      ${if isDarwin then ''sound=$(printf '%s\n' "$line" | jq -r '.sound // ""')'' else ""}
      session_id=$(printf '%s\n' "$line" | jq -r '.session_id // ""')

      # Check focus - skip notification if terminal is focused (unless WRIX_NOTIFY_ALWAYS=1)
      if [ "''${WRIX_NOTIFY_ALWAYS:-}" != "1" ] && [ -n "$session_id" ]; then
        if check_terminal_focused "$session_id"; then
          [ "$VERBOSE" = "1" ] && echo "notifyd: suppressed (terminal focused)" >&2
          continue
        fi
      fi

      ${notifyCmd}
    done
  '';

in
pkgs.writeShellApplication {
  name = "wrix-notifyd";
  runtimeInputs =
    with pkgs;
    [
      coreutils
      jq
      socat
    ]
    ++ (if isDarwin then [ terminal-notifier ] else [ libnotify ]);

  text = ''
    SOCKET="''${XDG_RUNTIME_DIR:-$HOME/.local/share}/wrix/notify.sock"
    mkdir -p "$(dirname "$SOCKET")"
    rm -f "$SOCKET"
    trap 'rm -f "$SOCKET"' EXIT

    # Write handler script to temp file (socat SYSTEM can't use exported functions)
    HANDLER_SCRIPT=$(mktemp)
    trap 'rm -f "$SOCKET" "$HANDLER_SCRIPT"' EXIT
    cat > "$HANDLER_SCRIPT" << 'HANDLER_EOF'
    ${handlerScript}
    HANDLER_EOF
    chmod +x "$HANDLER_SCRIPT"

    ${
      if isDarwin then
        ''
          # Darwin: listen on TCP (for containers) and Unix socket (for local testing)
          # Containers reach host via vmnet gateway (typically 192.168.64.1)
          echo "wrix-notifyd: listening on TCP port ${tcpPort} and $SOCKET"
          socat UNIX-LISTEN:"$SOCKET",fork EXEC:"bash $HANDLER_SCRIPT" &
          SOCKET_PID=$!
          trap 'rm -f "$SOCKET" "$HANDLER_SCRIPT"; kill $SOCKET_PID 2>/dev/null' EXIT
          # Bind to vmnet interface only (not all interfaces) for security
          # Containers see host as 192.168.64.1; binding there limits exposure
          socat TCP-LISTEN:${tcpPort},bind=192.168.64.1,fork,reuseaddr EXEC:"bash $HANDLER_SCRIPT"
        ''
      else
        ''
          # Linux: listen on Unix socket only (mounted into containers)
          echo "wrix-notifyd: listening on $SOCKET"
          socat UNIX-LISTEN:"$SOCKET",fork EXEC:"bash $HANDLER_SCRIPT"
        ''
    }
  '';
}
