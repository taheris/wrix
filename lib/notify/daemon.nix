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
  inherit (pkgs.stdenv.hostPlatform) isDarwin;
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
          if [[ -z "$session_app" ]]; then
            if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: no terminal_app in session file" >&2; fi
            return 1
          fi

          local focused_app
          if ! focused_app=$(osascript -e 'tell application "System Events" to name of first process whose frontmost is true' 2>/dev/null); then
            if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: osascript failed" >&2; fi
            return 1
          fi

          if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: app session=$session_app focused=$focused_app" >&2; fi
          [[ "$focused_app" == "$session_app" ]]
        }
      ''
    else
      ''
        check_app_focused() {
          local session_file="$1"
          if ! command -v niri >/dev/null 2>&1; then
            if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: niri unavailable for window focus check" >&2; fi
            return 1
          fi

          local session_window_id
          session_window_id=$(jq -r '.window_id // ""' "$session_file")
          if [[ -z "$session_window_id" ]]; then
            if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: no window_id in session file" >&2; fi
            return 1
          fi

          local focused_window_id
          if ! focused_window_id=$(niri msg -j focused-window 2>/dev/null | jq -r '.id // ""'); then
            if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: niri focused-window query failed" >&2; fi
            return 1
          fi

          if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: window session=$session_window_id focused=$focused_window_id" >&2; fi
          [[ "$focused_window_id" == "$session_window_id" ]]
        }
      '';

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

    ${sessionDir}
    VERBOSE="''${WRIX_NOTIFY_VERBOSE:-0}"

    ${appCheckFn}

    check_terminal_focused() {
      local focus_target="$1"
      local safe_id
      safe_id=$(printf '%s' "$focus_target" | LC_ALL=C tr -c 'A-Za-z0-9_-' '-')
      local session_file="$WRIX_SESSION_DIR/$safe_id.json"

      if [[ ! -f "$session_file" ]]; then
        if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: session file not found: $session_file" >&2; fi
        return 1
      fi

      if ! jq -e --arg target "$focus_target" '.focus_target == $target' "$session_file" >/dev/null; then
        if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: registration target mismatch" >&2; fi
        return 1
      fi
      if ! check_app_focused "$session_file"; then
        return 1
      fi

      local tmux_target
      tmux_target=$(jq -r '.tmux_target // ""' "$session_file")
      if [[ -n "$tmux_target" ]]; then
        if ! command -v tmux >/dev/null 2>&1; then
          if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: tmux unavailable for pane focus check" >&2; fi
          return 1
        fi
        local tmux_socket
        tmux_socket=$(jq -r '.tmux_socket // ""' "$session_file")
        local -a tmux_args=()
        if [[ -n "$tmux_socket" ]]; then tmux_args=(-S "$tmux_socket"); fi
        local pane_state
        if ! pane_state=$(tmux "''${tmux_args[@]}" display-message -p -t "$tmux_target" \
          '#{session_name}:#{window_index}.#{pane_index} #{window_active} #{pane_active} #{?session_attached,1,0}' 2>/dev/null); then
          if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: tmux target pane query failed" >&2; fi
          return 1
        fi
        if [[ "$VERBOSE" == "1" ]]; then echo "notifyd: pane target=$tmux_target state=$pane_state" >&2; fi
        if [[ "$pane_state" != "$tmux_target 1 1 1" ]]; then
          return 1
        fi
      fi

      return 0
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

      ${notifyCmd}
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
