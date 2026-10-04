#!/usr/bin/env bash
set -euo pipefail

# Test-only start-clock coordination; every date result comes from the real tool.
audit_start_clock() {
  local real_date="$1"
  shift
  local barrier="${WRIX_TEST_AUDIT_START_BARRIER:-}" member

  if [[ "$#" -eq 1 && "$1" = +%s && -n "$barrier" ]]; then
    member="${WRIX_TEST_AUDIT_START_MEMBER:-}"
    case "$member" in
      first|second) ;;
      *) printf 'audit clock: expected first or second member\n' >&2; return 64 ;;
    esac
    if [[ ! -d "$barrier/claimed-$member" ]]; then
      mkdir "$barrier/claimed-$member"
      : >"$barrier/ready-$member"
      while [[ ! -f "$barrier/release" ]]; do
        sleep 0.01
      done
    fi
  fi
  exec "$real_date" "$@"
}

audit_wait_for_start_clocks() {
  local barrier="$1" first_pid="$2" second_pid="$3"
  local deadline=$((SECONDS + 120))

  # Bound only readiness; no audit execution timeout or retry policy is changed.
  while [[ ! -f "$barrier/ready-first" || ! -f "$barrier/ready-second" ]]; do
    if ! kill -0 "$first_pid" || ! kill -0 "$second_pid"; then
      printf 'audit clock: a launcher exited before both start clocks were ready\n' >&2
      return 1
    fi
    if [[ "$SECONDS" -ge "$deadline" ]]; then
      printf 'audit clock: both entrypoint start clocks were not ready within 120s\n' >&2
      return 1
    fi
    sleep 0.01
  done
}

audit_release_start_clocks() {
  local barrier="$1" current_second

  current_second=$(date +%s)
  while [[ "$(date +%s)" = "$current_second" ]]; do
    sleep 0.01
  done
  : >"$barrier/release"
}

if [[ "${BASH_SOURCE[0]}" = "$0" ]]; then
  audit_start_clock "$@"
fi
