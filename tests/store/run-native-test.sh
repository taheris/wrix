#!/usr/bin/env bash
# Keep resource cleanup outside the test process, including timeout/crash paths.
set -euo pipefail
: "${WRIX_TEST_POOL_PYTHON:?}"
: "${WRIX_TEST_POOL_CLEANUP_SCRIPT:?}"
registry=$(mktemp -d)
export WRIX_TEST_POOL_FIXTURE_REGISTRY="$registry"
cleanup_native_test() {
  local status=$?
  trap - EXIT INT TERM
  local -a args=(--registry "$registry")
  if [[ "$status" == 0 ]]; then
    args+=(--remove-files)
  else
    args+=(--diagnostics)
  fi
  if [[ -n "${WRIX_TEST_POOL_CLEANUP_TOOLS:-}" ]]; then
    args+=(--tools "$WRIX_TEST_POOL_CLEANUP_TOOLS")
  fi
  if "$WRIX_TEST_POOL_PYTHON" "$WRIX_TEST_POOL_CLEANUP_SCRIPT" "${args[@]}"; then
    if [[ "$status" == 0 ]]; then
      rmdir "$registry" || status=1
    else
      printf 'native failure diagnostics and registry retained: %s\n' "$registry" >&2
    fi
  else
    printf 'native test cleanup incomplete; retained registry: %s\n' "$registry" >&2
    status=1
  fi
  exit "$status"
}
test_pid=
terminate_native_test() {
  local status=$1 pid=${test_pid:-${!:-}}
  trap - INT TERM
  if [[ -n "$pid" ]]; then
    # timeout forwards TERM to its separate process group and bounds the wait.
    kill -TERM "$pid" 2>/dev/null || true # best-effort: the owned timeout child may already have exited.
    wait "$pid" 2>/dev/null || true # Its expected signal/failure exit must not replace the received signal status.
  fi
  exit "$status"
}
trap cleanup_native_test EXIT
trap 'terminate_native_test 130' INT
trap 'terminate_native_test 143' TERM
timeout --kill-after=10s "${WRIX_TEST_POOL_TEST_TIMEOUT:-15m}" "$@" &
test_pid=$!
wait "$test_pid"
