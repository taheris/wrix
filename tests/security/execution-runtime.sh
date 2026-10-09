#!/usr/bin/env bash
set -euo pipefail

# Test-only naming and pre-run gate; all runtime operations reach the real CLI.
if [[ "${1:-}" == run ]]; then
  shift
  printf '%s\n' "$PPID" >"$WRIX_TEST_CONTROL/host-pid"
  printf '%s\n' "$$" >"$WRIX_TEST_CONTROL/runtime-pid"
  touch "$WRIX_TEST_CONTROL/ready"
  deadline=$((SECONDS + 120))
  while [[ ! -f "$WRIX_TEST_CONTROL/release" ]]; do
    if [[ "$SECONDS" -ge "$deadline" ]]; then
      printf 'execution runtime: pre-run gate timed out\n' >&2
      exit 124
    fi
    sleep 0.05
  done
  exec "$WRIX_TEST_REAL_RUNTIME" run --name "$WRIX_TEST_CONTAINER_NAME" "$@"
fi
exec "$WRIX_TEST_REAL_RUNTIME" "$@"
