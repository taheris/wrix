#!/usr/bin/env bash
set -euo pipefail

if [[ "$*" == --version ]]; then
  printf '%s\n' 'podman version 6.1.4'
  exit 0
fi
if [[ "$*" == 'info --format json' ]]; then
  if [[ -n "${WRIX_TEST_RUNTIME_PREFLIGHT_ERROR:-}" ]]; then
    printf '%s\n' "$WRIX_TEST_RUNTIME_PREFLIGHT_ERROR" >&2
    exit 125
  fi
  printf '%s\n' '{"version":{"Version":"6.1.4"},"host":{"security":{"rootless":true}},"store":{"graphRoot":"/home/test/containers/storage","runRoot":"/run/user/1000/containers"}}'
  exit 0
fi


state="${WRIX_TEST_RUNTIME_STATE:?}"
case "${1:-}" in
  image)
    [[ "${2:-}" == "inspect" ]]
    format="${4:-}"
    case "$format" in
      '{{.Id}}') printf 'sha256:%064d\n' 0 ;;
      '{{.Digest}}') printf 'sha256:%064d\n' 0 ;;
      '{{ index .Config.Labels "wrix.nix.private" }}' | '{{ index .Config.Labels "wrix.runtime.environment" }}') printf '1\n' ;;
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
          elif [[ "$volume" == *:/run/wrix/runtime-environment:ro ]]; then
            exec 4<"${volume%:/run/wrix/runtime-environment:ro}/environment"
            IFS= read -r -d '' magic <&4
            [[ "$magic" == wrix-runtime-env-v1 ]]
            while IFS= read -r -d '' name <&4; do
              IFS= read -r -d '' value <&4
              [[ "$name" != WRIX_SPAWN_CONFIG ]] || config_env="$value"
            done
            exec 4<&-
          fi
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
