#!/usr/bin/env bash
set -euo pipefail

# Judge rubrics for notifications.md success criteria

test_native_dispatch_and_reliability() {
  judge_files "lib/notify/daemon.nix"
  judge_criterion "The host daemon dispatches through native desktop notification bridges and remains available after client disconnects. PASS if the Linux path invokes notify-send or an equivalent libnotify command, the macOS path invokes terminal-notifier with the client-provided sound when present, and the listener setup forks or otherwise handles independent connections so one client disconnect does not require a daemon restart."
}
