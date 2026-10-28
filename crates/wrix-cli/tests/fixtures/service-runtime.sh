#!/usr/bin/env bash
set -euo pipefail

runtime="${0##*/}"
# Each external engine owns separate named containers and immutable launch recipes.
records="${WRIX_TEST_RUNNING:?}.owners/$runtime"
mkdir -p "$records"
selected="${!#}"
[[ "$selected" != --type=container ]] || selected="${4:-}"
recipe="$records/$selected"

printf '%s\0' "$@" >>"${WRIX_TEST_CALLS:?}"
snapshot() {
  "${WRIX_TEST_JQ_BIN:?}" --arg image "${WRIX_TEST_IMAGE_ID:-$WRIX_TEST_DIGEST}" -Rsc '
    split("\u0000")[:-1] as $args |
    def values($flag): [range(0; ($args | length) - 1) as $i |
      select($args[$i] == $flag) | $args[$i + 1]];
    (values("--label") | map(split("=") |
      {key: .[0], value: (.[1:] | join("="))}) | from_entries) as $labels |
    (values("-p") | map(split(":") | {
      hostAddress: .[0], hostPort: (.[1] | tonumber),
      containerPort: (.[2] | tonumber), proto: "tcp", count: 1
    })) as $ports |
    [{configuration: {id: values("--name")[0], labels: $labels, publishedPorts: $ports,
        image: {descriptor: {digest: (if $args[-4] | startswith("sha256:") then $args[-4] else $image end)}}},
      status: {state: "running"}}]
  ' "$recipe"
}
is_running() {
  [[ -f "$recipe" ]] || return 1
  [[ ! -L "$recipe" || -f "${WRIX_TEST_RUNNING:?}" ]]
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
      if [[ "${!#}" == sha256:* ]]; then
        printf 'unsupported fixture: Apple image lookup requires a stored reference\n' >&2
        exit 64
      fi
      printf '[{"id":"%s","digest":"%s","labels":{"wrix.nix.private":"%s"}}]\n' "${WRIX_TEST_IMAGE_ID:-$WRIX_TEST_DIGEST}" "${WRIX_TEST_CONTENT_DIGEST:-$WRIX_TEST_DIGEST}" "${WRIX_TEST_PRIVATE_NIX_VERSION:-1}"
    elif [[ "${4:-}" == '{{ index .Config.Labels "wrix.nix.private" }}' ]]; then
      printf '%s\n' "${WRIX_TEST_PRIVATE_NIX_VERSION:-1}"
    elif [[ "${4:-}" == '{{.Id}}' ]]; then
      printf '%s\n' "${WRIX_TEST_IMAGE_ID:-$WRIX_TEST_DIGEST}"
    else
      printf '%s\n' "${WRIX_TEST_CONTENT_DIGEST:-$WRIX_TEST_DIGEST}"
    fi
    ;;
  'image exists') [[ -f "${WRIX_TEST_IMAGE:?}" ]] ;;
  'image list')
    [[ "$*" == 'image list --format json' ]]
    if [[ -f "${WRIX_TEST_IMAGE:?}" ]]; then
      printf '[{"configuration":{"name":"wrix-service:test"},"id":"%s"}]\n' "${WRIX_TEST_IMAGE_ID:-$WRIX_TEST_DIGEST}"
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
    for recipe in "$records"/*; do
      is_running || continue
      snapshot | "${WRIX_TEST_JQ_BIN:?}" -r '.[].configuration.id'
    done
    ;;
  'list --all')
    for recipe in "$records"/*; do
      is_running || continue
      snapshot
    done | "${WRIX_TEST_JQ_BIN:?}" -s 'add // []'
    ;;
  'container exists') is_running ;;
  inspect*)
    if ! is_running; then
      printf 'Error: no such object\n' >&2
      exit 1
    fi
    name="${!#}"
    if [[ "$name" == --type=container ]]; then
      name="$4"
    fi
    if ! snapshot | "${WRIX_TEST_JQ_BIN:?}" -e --arg name "$name" 'any(.[]; .configuration.id == $name)' >/dev/null; then
      printf 'Error: no such object\n' >&2
      exit 1
    fi
    if [[ "$runtime" == container ]]; then
      snapshot
    elif [[ "${3:-}" == '{{.State.Running}}' ]]; then
      printf 'true\n'
    elif [[ "${3:-}" == '{{.Image}}' ]]; then
      snapshot | "${WRIX_TEST_JQ_BIN:?}" -r '.[0].configuration.image.descriptor.digest'
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
    name=""
    previous=""
    for argument in "$@"; do
      [[ "$previous" != --name ]] || name="$argument"
      previous="$argument"
    done
    [[ "$name" =~ ^[a-zA-Z0-9_-]+$ ]]
    recipe="$records/$name"
    rm -f "$recipe"
    printf '%s\0' "$@" >"$recipe"
    if [[ "${!#}" != gateway ]]; then
      cp "$recipe" "${WRIX_TEST_RUN_ARGS:?}"
      rm "$recipe"
      ln -s "${WRIX_TEST_RUN_ARGS:?}" "$recipe"
      printf '%s\n' "$runtime" >"${WRIX_TEST_RUNNING:?}"
    else
      native_gateway "$@"
    fi
    ;;
  *) printf 'unexpected service runtime arguments: %s\n' "$*" >&2; exit 64 ;;
esac
