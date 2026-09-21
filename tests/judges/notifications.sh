#!/usr/bin/env bash
set -euo pipefail

# Judge rubrics for notifications.md success criteria

test_focus_suppression() {
  judge_files "lib/notify/daemon.nix"
  judge_criterion "The daemon suppresses a notification only after it can positively determine that the registered session target is focused. PASS if missing session files, missing terminal_app/window_id values, unavailable focus tools, or failed focus queries all allow the notification to be shown instead of being treated as focused. PASS if a positive app/window match still suppresses after the tmux pane check does not identify a different active pane."
}

test_native_dispatch_and_reliability() {
  judge_files "lib/notify/daemon.nix"
  judge_criterion "The host daemon dispatches through native desktop notification bridges and remains available after client disconnects. PASS if the Linux path invokes notify-send or an equivalent libnotify command, the macOS path invokes terminal-notifier with the client-provided sound when present, and the listener setup forks or otherwise handles independent connections so one client disconnect does not require a daemon restart."
}

test_session_registration() {
  judge_files "crates/wrix-sandbox/src/command/launch.rs" "lib/notify/daemon.nix"
  judge_criterion "The live Rust launcher registers focus targets for tmux sessions in the runtime session directory before starting the container, passes the derived WRIX_SESSION_ID into the container, and removes the current session file after launch. The daemon must read that file with the same session_id filename normalization. PASS if Linux records window_id when available, macOS records terminal_app when available, and both records include session_id. PASS only if overlapping launches from the same tmux pane retain the shared registration until the final launch exits through a reference count or an ownership-safe equivalent."
}
