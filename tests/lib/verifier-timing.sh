#!/usr/bin/env bash
set -euo pipefail

# Optional diagnostics never change verdicts or expose command arguments.
verifier_timed() {
  local phase="$1" target="$2" start="$SECONDS" status=0
  shift 2
  "$@" || status=$?
  if [[ -n "${WRIX_VERIFIER_TIMING_FILE:-}" ]]; then
    if ! printf '%s\t%s\t%s\t%s\t%s\n' "$phase" "$target" "$((SECONDS-start))" "$status" "$BASHPID" >> "$WRIX_VERIFIER_TIMING_FILE"; then
      printf 'verifier timing output unavailable\n' >&2
    fi
  fi
  return "$status"
}
