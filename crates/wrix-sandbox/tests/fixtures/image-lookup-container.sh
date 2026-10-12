#!/usr/bin/env bash
set -euo pipefail

state="${WRIX_TEST_IMAGE_LOOKUP_STATE:?}"
printf '%s\n' "$*" >>"$state/calls"
case "${1:-} ${2:-}" in
  'image list')
    if [[ "$*" == 'image list --format json' ]]; then
      cat "$state/list.json"
    elif [[ "$*" == 'image list' ]]; then
      cat "$state/list.txt"
    else
      exit 91
    fi
    ;;
  'image inspect')
    [[ $# == 3 && -f "$state/inspect/$3.json" ]] || exit 1
    cat "$state/inspect/$3.json"
    ;;
  'image tag') [[ $# == 4 ]] ;;
  *) printf 'unexpected container arguments: %s\n' "$*" >&2; exit 91 ;;
esac
