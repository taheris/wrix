#!/usr/bin/env bash
set -euo pipefail

state="${WRIX_TEST_RUNTIME_STATE:?}"
case "${1:-} ${2:-}" in
  'image list')
    [[ "$*" == 'image list --format json' ]]
    printf '%s\n' '[{"configuration":{"name":"localhost/wrix-test:latest"},"id":"sha256:image-id"}]'
    ;;
  'image inspect')
    printf '%s\n' '[{"id":"sha256:image-id","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","labels":{"wrix.managed":"true","wrix.nix.private":"1"}}]'
    ;;
  run*)
    shift
    config_host=""
    config_env=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        -v)
          volume="${2:?}"
          if [[ "$volume" == *:/mnt/wrix/spawn-config:ro ]]; then
            config_host="${volume%:/mnt/wrix/spawn-config:ro}/spawn-config.json"
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
    [[ "$config_env" == "/mnt/wrix/spawn-config/spawn-config.json" ]]
    [[ -n "$config_host" ]]
    WRIX_SPAWN_CONFIG="$config_host" "${WRIX_TEST_CONSUMER_ENTRYPOINT:?}"
    ;;
  *) ;;
esac
