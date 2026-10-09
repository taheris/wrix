#!/usr/bin/env bash
set -euo pipefail

runtime="${0##*/}"
case "${1:-} ${2:-}" in
  '-n get')
    [[ "$runtime" == route && "$#" == 3 && "$3" == default ]] || exit 91
    printf '%s\n' 'interface: en0'
    ;;
  'image inspect')
    if [[ "$runtime" == container ]]; then
      printf '[{"id":"%s","digest":"%s"}]\n' "${WRIX_TEST_DIGEST:?}" "$WRIX_TEST_DIGEST"
    else
      printf '%s\n' "${WRIX_TEST_DIGEST:?}"
    fi
    ;;
  'images --format' | 'ps -a') ;;
  'image list' | 'list --all') printf '%s\n' '[]' ;;
  tag*) [[ "${2:-}" == "${WRIX_TEST_DIGEST:?}" ]] ;;
  'image tag') [[ "${3:-}" == "${WRIX_TEST_DIGEST:?}" ]] ;;
  'run --rm')
    printf '%s\0' "$@" >"${WRIX_TEST_ARGV:?}"
    exec 3<>"/dev/tcp/127.0.0.1/${WRIX_TEST_CONTROL_PORT:?}"
    printf '%s\n' "ready $runtime" >&3
    while IFS= read -r -t "${WRIX_TEST_CONTROL_TIMEOUT:-15}" request <&3; do
      case "$request" in
        probe) printf '%s\n' held >&3 ;;
        release)
          printf 'finished %s\n' "${WRIX_TEST_EXIT_CODE:?}" >&3
          exit "$WRIX_TEST_EXIT_CODE"
          ;;
        *) printf 'unexpected control request: %s\n' "$request" >&2; exit 91 ;;
      esac
    done
    printf '%s\n' 'runtime control disconnected or timed out before release' >&2
    exit 92
    ;;
  *) printf 'unexpected runtime arguments: %s\n' "$*" >&2; exit 91 ;;
esac
