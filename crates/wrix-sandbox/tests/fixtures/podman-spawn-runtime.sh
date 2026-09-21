#!/usr/bin/env bash
set -euo pipefail

state="${WRIX_TEST_RUNTIME_STATE:?}"
case "${1:-}" in
  image)
    [[ "${2:-}" == "inspect" ]]
    format="${4:-}"
    case "$format" in
      '{{.Id}}') printf 'sha256:%064d\n' 0 ;;
      '{{.Digest}}') printf 'sha256:%064d\n' 0 ;;
      *) printf 'true\n' ;;
    esac
    ;;
  images) ;;
  tag) ;;
  run)
    shift
    config_host=""
    config_env=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        -v)
          volume="${2:?}"
          if [[ "$volume" == *:/run/wrix/spawn-config.json:ro ]]; then
            config_host="${volume%:/run/wrix/spawn-config.json:ro}"
            : >"$state/read-only-mount"
          fi
          shift 2
          ;;
        -e)
          pair="${2:?}"
          [[ "$pair" != WRIX_SPAWN_CONFIG=* ]] || config_env="${pair#WRIX_SPAWN_CONFIG=}"
          shift 2
          ;;
        *) shift ;;
      esac
    done
    [[ "$config_env" == "/run/wrix/spawn-config.json" ]]
    [[ -n "$config_host" ]]
    WRIX_SPAWN_CONFIG="$config_host" "${WRIX_TEST_CONSUMER_ENTRYPOINT:?}"
    ;;
  *) ;;
esac
