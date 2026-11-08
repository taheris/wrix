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

if [[ -n "${WRIX_TEST_EXPECT_CONTAINER_HOST:-}" ]]; then
  [[ "${CONTAINER_HOST-}" == "$WRIX_TEST_EXPECT_CONTAINER_HOST" ]] || exit 90
fi

runtime="$(basename "$0")"
if [[ "$runtime" == "route" ]]; then
  [[ "$*" == '-n get default' ]] || exit 91
  printf '%s\n' 'interface: en0'
  exit 0
fi

case "${1:-} ${2:-}" in
  'image inspect')
    if [[ "$runtime" == "container" ]]; then
      printf '[{"id":"%s","digest":"%s","labels":{"wrix.nix.private":"%s","wrix.runtime.environment":"%s"}}]\n' "${WRIX_TEST_DIGEST:?}" "$WRIX_TEST_DIGEST" "${WRIX_TEST_PRIVATE_NIX_VERSION:-1}" "${WRIX_TEST_RUNTIME_ENV_VERSION-1}"
    elif [[ "${4:-}" == '{{ index .Config.Labels "wrix.nix.private" }}' ]]; then
      printf '%s\n' "${WRIX_TEST_PRIVATE_NIX_VERSION:-1}"
    elif [[ "${4:-}" == '{{ index .Config.Labels "wrix.runtime.environment" }}' ]]; then
      printf '%s\n' "${WRIX_TEST_RUNTIME_ENV_VERSION-1}"
    else
      printf '%s\n' "${WRIX_TEST_DIGEST:?}"
    fi
    ;;
  'images --format' | 'ps -a') ;;
  'image list')
    printf '[{"configuration":{"name":"localhost/wrix-test:latest"},"id":"%s"}]\n' "${WRIX_TEST_DIGEST:?}"
    ;;
  'list --all') printf '%s\n' '[]' ;;
  tag*) [[ "${2:-}" == "${WRIX_TEST_DIGEST:?}" ]] ;;
  'image tag') [[ "${3:-}" == "${WRIX_TEST_DIGEST:?}" ]] ;;
  'run --rm')
    printf '%s\0' "$@" >"${WRIX_TEST_ARGV:?}"
    for arg in "$@"; do
      if [[ "$arg" == *:/run/wrix/runtime-environment:ro ]]; then
        cp "${arg%:/run/wrix/runtime-environment:ro}/environment" "$WRIX_TEST_ARGV.environment"
      fi
    done
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
