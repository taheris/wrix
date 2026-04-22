#!/usr/bin/env bash
# Shared poll_until helper for integration tests.
#
# Usage:
#   source tests/lib/poll.sh
#   poll_until "test -S /path/to/socket" 30
#   poll_until "curl -sf http://localhost:8080" 10 0.5
#
# Optional environment variables (set before calling):
#   POLL_WATCH_PID  — PID whose death aborts the poll (e.g. a server under test)
#   POLL_WATCH_LOG  — log file to tail when POLL_WATCH_PID dies
#
# Returns 0 on success, 1 on timeout or watched process death.

poll_until() {
  local cmd="$1"
  local timeout="${2:-30}"
  local interval="${3:-1}"
  local elapsed=0
  echo "  > waiting (up to ${timeout}s): $cmd"
  while [ "$elapsed" -lt "$timeout" ]; do
    if eval "$cmd" >/dev/null 2>&1; then
      echo "  > satisfied after ${elapsed}s"
      return 0
    fi
    if [ -n "${POLL_WATCH_PID:-}" ] && ! kill -0 "$POLL_WATCH_PID" 2>/dev/null; then
      echo "  > watched pid $POLL_WATCH_PID died during poll"
      if [ -n "${POLL_WATCH_LOG:-}" ] && [ -f "$POLL_WATCH_LOG" ]; then
        echo "  ${POLL_WATCH_LOG} tail:"
        tail -20 "$POLL_WATCH_LOG" | sed 's/^/    /'
      fi
      return 1
    fi
    sleep "$interval"
    elapsed=$((elapsed + interval))
  done
  echo "  > TIMED OUT after ${timeout}s: $cmd"
  return 1
}
