#!/usr/bin/env bash
set -euo pipefail

# Judge rubrics for notifications.md success criteria

test_focus_suppression() {
  judge_files "lib/notify/daemon.nix"
  judge_criterion "The daemon suppresses a notification only after it can positively determine that the registered session target is focused. PASS if missing session files, missing terminal_app/window_id values, unavailable focus tools, or failed focus queries all allow the notification to be shown instead of being treated as focused. PASS if a positive app/window match still suppresses after the tmux pane check does not identify a different active pane."
}

test_concurrent_connections() {
  judge_files "lib/notify/daemon.nix"
  judge_criterion "Daemon handles multiple simultaneous client connections without blocking or dropping messages"
}

test_linux_notify_send() {
  judge_files "lib/notify/daemon.nix"
  judge_criterion "On Linux, the daemon delivers desktop notifications via notify-send (or an equivalent libnotify-backed command) when a client posts a message. PASS if the Linux code path in the daemon shells out to notify-send (or a clearly equivalent libnotify tool) to render the notification."
}

test_macos_terminal_notifier() {
  judge_files "lib/notify/daemon.nix"
  judge_criterion "On macOS, the daemon delivers desktop notifications via terminal-notifier (or an osascript-based fallback that triggers a native macOS notification). PASS if the macOS code path invokes terminal-notifier or an osascript 'display notification' command."
}

test_native_dispatch_and_reliability() {
  judge_files "lib/notify/daemon.nix"
  judge_criterion "The host daemon dispatches through native desktop notification bridges and remains available after client disconnects. PASS if the Linux path invokes notify-send or an equivalent libnotify command, the macOS path invokes terminal-notifier with the client-provided sound when present, and the listener setup forks or otherwise handles independent connections so one client disconnect does not require a daemon restart."
}

test_session_registration() {
  judge_files "lib/sandbox/linux/default.nix" "lib/sandbox/darwin/default.nix" "lib/notify/daemon.nix"
  judge_criterion "The launchers register focus targets for tmux sessions in the runtime session directory before starting the container, and the daemon reads those files using the same safe session_id filename scheme. PASS if Linux records a window_id when available, macOS records the frontmost terminal_app when available, both records include session_id, and cleanup removes only the current session file."
}

test_client_in_container() {
  judge_files "lib/notify/client.nix" "lib/sandbox/linux/default.nix" "lib/sandbox/darwin/default.nix"
  judge_criterion "The notify client works from inside a wrix container: it connects to the host-running daemon via a transport that is reachable from inside the sandbox (Unix socket bind-mounted into the container on Linux, TCP on the vmnet bridge on Darwin). PASS if the client code and launcher scripts together wire up that transport so a containerized call reaches the host daemon."
}
