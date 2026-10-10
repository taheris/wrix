#!/usr/bin/env bash
set -euo pipefail

runtime="${0##*/}"
printf '%s\0' "$@" >>"${WRIX_TEST_CALLS:?}"
if [[ "$runtime" == skopeo ]]; then
  case "${1:-} ${2:-}" in
    'inspect --raw')
      source="${3:-}"
      [[ "$source" == docker-archive:* && -f "${source#docker-archive:}" ]]
      printf '{"config":{"digest":"%s"}}\n' "${WRIX_TEST_DIGEST:?}"
      ;;
    '--insecure-policy copy')
      source="${4:-}"
      case "$source" in
        oci:*) layout="${source#oci:}"; [[ -d "${layout%:*}" ]] ;;
        docker-archive:*) [[ -f "${source#docker-archive:}" ]] ;;
        *) printf 'unexpected copy source: %s\n' "$source" >&2; exit 64 ;;
      esac
      destination="${!#}"
      case "$destination" in
        containers-storage:*) touch "${WRIX_TEST_IMAGE:?}" ;;
        oci-archive:*) touch "${destination#oci-archive:}" ;;
        *) printf 'unexpected copy destination: %s\n' "$destination" >&2; exit 64 ;;
      esac
      ;;
    *) printf 'unexpected skopeo arguments: %s\n' "$*" >&2; exit 64 ;;
  esac
  exit 0
fi
case "${1:-} ${2:-}" in
  'image inspect')
    [[ -f "${WRIX_TEST_IMAGE:?}" ]] || exit 1
    if [[ "$runtime" == container ]]; then
      printf '[{"id":"%s","digest":"%s"}]\n' "${WRIX_TEST_DIGEST:?}" "$WRIX_TEST_DIGEST"
    else
      printf '%s\n' "${WRIX_TEST_DIGEST:?}"
    fi
    ;;
  'image exists') [[ -f "${WRIX_TEST_IMAGE:?}" ]] ;;
  'image list')
    if [[ "${3:-}" == --format ]]; then
      printf '[]\n'
    else
      printf 'NAME TAG DIGEST\n'
      [[ ! -f "${WRIX_TEST_IMAGE:?}" ]] || printf 'wrix-service test %s\n' "${WRIX_TEST_DIGEST:?}"
    fi
    ;;
  tag* | 'image tag') [[ -f "${WRIX_TEST_IMAGE:?}" ]] ;;
  'image load')
    [[ "${3:-}" == --input && -f "${4:-}" ]]
    touch "${WRIX_TEST_IMAGE:?}"
    printf 'Loaded: untagged@%s\n' "${WRIX_TEST_DIGEST:?}"
    ;;
  'image delete') [[ "${3:-}" == untagged@* ]] ;;
  'info --format') printf 'overlay@/tmp/wrix-fixture-store+/tmp/wrix-fixture-runroot\n' ;;
  'images --format' | 'ps -a') ;;
  'list --all')
    if [[ -f "${WRIX_TEST_RUNNING:?}" ]]; then
      printf '[{"configuration":{"id":"workspace-service"},"status":{"state":"running"}}]\n'
    else
      printf '[]\n'
    fi
    ;;
  'container exists') [[ -f "${WRIX_TEST_RUNNING:?}" ]] ;;
  inspect*)
    if [[ ! -f "${WRIX_TEST_RUNNING:?}" ]]; then
      printf 'Error: no such object\n' >&2
      exit 1
    fi
    if [[ "$runtime" == container ]]; then
      printf '[{"configuration":{"id":"workspace-service"},"status":{"state":"running"}}]\n'
    else
      printf 'true\n'
    fi
    ;;
  'run -d')
    printf '%s\0' "$@" >"${WRIX_TEST_RUN_ARGS:?}"
    touch "${WRIX_TEST_RUNNING:?}"
    ;;
  *) printf 'unexpected service runtime arguments: %s\n' "$*" >&2; exit 64 ;;
esac
