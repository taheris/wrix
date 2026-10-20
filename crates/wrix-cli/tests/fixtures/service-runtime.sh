#!/usr/bin/env bash
set -euo pipefail

runtime="${0##*/}"
printf '%s\0' "$@" >>"${WRIX_TEST_CALLS:?}"
snapshot() {
  "${WRIX_TEST_JQ_BIN:?}" -Rsc '
    split("\u0000")[:-1] as $args |
    def values($flag): [range(0; ($args | length) - 1) as $i |
      select($args[$i] == $flag) | $args[$i + 1]];
    (values("--label") | map(split("=") |
      {key: .[0], value: (.[1:] | join("="))}) | from_entries) as $labels |
    (values("-p") | map(split(":") | {
      hostAddress: .[0], hostPort: (.[1] | tonumber),
      containerPort: (.[2] | tonumber), proto: "tcp", count: 1
    })) as $ports |
    [{configuration: {id: values("--name")[0], labels: $labels, publishedPorts: $ports},
      status: {state: "running"}}]
  ' "${WRIX_TEST_RUN_ARGS:?}"
}
is_running() {
  local owner=''
  [[ -f "${WRIX_TEST_RUNNING:?}" ]] || return 1
  IFS= read -r owner <"$WRIX_TEST_RUNNING" || return 1
  [[ "$owner" == "$runtime" ]]
}
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
      printf '[{"id":"%s","digest":"%s","labels":{"wrix.nix.private":"%s"}}]\n' "${WRIX_TEST_DIGEST:?}" "$WRIX_TEST_DIGEST" "${WRIX_TEST_PRIVATE_NIX_VERSION:-1}"
    elif [[ "${4:-}" == '{{ index .Config.Labels "wrix.nix.private" }}' ]]; then
      printf '%s\n' "${WRIX_TEST_PRIVATE_NIX_VERSION:-1}"
    else
      printf '%s\n' "${WRIX_TEST_DIGEST:?}"
    fi
    ;;
  'image exists') [[ -f "${WRIX_TEST_IMAGE:?}" ]] ;;
  'image list')
    [[ "$*" == 'image list --format json' ]]
    if [[ -f "${WRIX_TEST_IMAGE:?}" ]]; then
      printf '[{"configuration":{"name":"wrix-service:test"},"id":"%s"}]\n' "${WRIX_TEST_DIGEST:?}"
    else
      printf '[]\n'
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
  'images --format') ;;
  'ps -a')
    if is_running; then
      snapshot | "${WRIX_TEST_JQ_BIN:?}" -r '.[].configuration.id'
    fi
    ;;
  'list --all')
    if is_running; then
      snapshot
    else
      printf '[]\n'
    fi
    ;;
  'container exists') is_running ;;
  inspect*)
    if ! is_running; then
      printf 'Error: no such object\n' >&2
      exit 1
    fi
    if ! snapshot | "${WRIX_TEST_JQ_BIN:?}" -e --arg name "${!#}" 'any(.[]; .configuration.id == $name)' >/dev/null; then
      printf 'Error: no such object\n' >&2
      exit 1
    fi
    if [[ "$runtime" == container ]]; then
      snapshot
    elif [[ "${3:-}" == '{{.State.Running}}' ]]; then
      printf 'true\n'
    elif [[ "${3:-}" == '{{ index .Config.Labels "'*'" }}' ]]; then
      key="${3#'{{ index .Config.Labels "'}"
      key="${key%'" }}'}"
      snapshot | "${WRIX_TEST_JQ_BIN:?}" -r --arg key "$key" '.[0].configuration.labels[$key] // ""'
    else
      printf 'unexpected service inspection: %s\n' "$*" >&2
      exit 64
    fi
    ;;
  port*)
    is_running
    snapshot | "${WRIX_TEST_JQ_BIN:?}" -r '.[0].configuration.publishedPorts[] | "\(.containerPort)/tcp -> \(.hostAddress):\(.hostPort)"'
    ;;
  'run -d')
    printf '%s\0' "$@" >"${WRIX_TEST_RUN_ARGS:?}"
    printf '%s\n' "$runtime" >"${WRIX_TEST_RUNNING:?}"
    ;;
  *) printf 'unexpected service runtime arguments: %s\n' "$*" >&2; exit 64 ;;
esac
