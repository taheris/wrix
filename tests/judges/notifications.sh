#!/usr/bin/env bash
# Judge rubrics for notifications.md success criteria

test_focus_suppression() {
  judge_files "lib/notify/daemon.nix"
  judge_criterion "No notification is sent when the terminal window is focused (focus-aware suppression is implemented)"
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

test_client_in_container() {
  judge_files "lib/notify/client.nix" "lib/sandbox/linux/entrypoint.sh" "lib/sandbox/darwin/entrypoint.sh"
  judge_criterion "The notify client works from inside a wrapix container: it connects to the host-running daemon via a transport that is reachable from inside the sandbox (Unix socket bind-mounted into the container on Linux, TCP on the vmnet bridge on Darwin). PASS if the client code and entrypoint scripts together wire up that transport so a containerized call reaches the host daemon."
}
