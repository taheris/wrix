#!/usr/bin/env bash
set -euo pipefail

runtime="$(basename "$0")"
if [[ "$runtime" == "route" ]]; then
  [[ "$*" == '-n get default' ]] || exit 91
  printf '%s\n' 'interface: en0'
  exit 0
fi

case "${1:-} ${2:-}" in
  'image inspect')
    if [[ "$runtime" == "container" ]]; then
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
    if [[ -n "${WRIX_TEST_KEYS:-}" ]]; then
      mkdir -p "$WRIX_TEST_KEYS"
      for arg in "$@"; do
        if [[ "$arg" == *:/etc/wrix/keys:ro ]]; then
          cp -R "${arg%:/etc/wrix/keys:ro}/." "$WRIX_TEST_KEYS/"
        fi
      done
    fi
    ;;
  *) printf 'unexpected runtime arguments: %s\n' "$*" >&2; exit 91 ;;
esac
