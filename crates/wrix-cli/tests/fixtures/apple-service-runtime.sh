#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${WRIX_TEST_RUNTIME_LOG:?}"
case "$*" in
  'image list --format json') printf '[{"configuration":{"name":"wrix-service:test"},"id":"sha256:%s"}]\n' "${WRIX_TEST_DIGEST:?}"; exit 0 ;;
  'image inspect '*)
    if [[ "${!#}" == sha256:* ]]; then
      printf 'unsupported fixture: Apple image lookup requires a stored reference\n' >&2
      exit 64
    fi
    printf '[{"id":"sha256:%s","digest":"sha256:%s"}]\n' "${WRIX_TEST_DIGEST:?}" "$WRIX_TEST_DIGEST"
    exit 0
    ;;
  'image tag '*) exit 0 ;;
  'list --all --format json') ;;
  'inspect '*)
    if [[ "$#" -ne 2 || "$2" == -* ]]; then
      printf 'unexpected Apple container command: %s\n' "$*" >&2
      exit 64
    fi
    ;;
  'rm -f '*)
    [[ "$#" -eq 3 ]]
    "${WRIX_TEST_JQ_BIN:?}" --arg name "$3" 'map(select(.configuration.id != $name))' \
      "${WRIX_TEST_RUNTIME_JSON:?}" >"${WRIX_TEST_RUNTIME_JSON}.new"
    mv "${WRIX_TEST_RUNTIME_JSON}.new" "$WRIX_TEST_RUNTIME_JSON"
    exit 0
    ;;
  'run -d --name '*)
    printf '%s\n' "$@" >"${WRIX_TEST_RUN_ARGS:?}"
    printf '%s\0' "$@" >"${WRIX_TEST_RUN_ARGS}.nul"
    "${WRIX_TEST_JQ_BIN:?}" --arg image "sha256:${WRIX_TEST_DIGEST:?}" -Rsc --slurpfile previous "${WRIX_TEST_RUNTIME_REPLACEMENT:?}" --slurpfile current "${WRIX_TEST_RUNTIME_JSON:?}" '
      split("\u0000")[:-1] as $args |
      def values($flag): [range(0; ($args | length) - 1) as $i |
        select($args[$i] == $flag) | $args[$i + 1]];
      values("--name")[0] as $name |
      (values("--label") | map(split("=") |
        {key: .[0], value: (.[1:] | join("="))}) | from_entries) as $labels |
      (values("-p") | map(split(":") | {
        hostAddress: .[0], hostPort: (.[1] | tonumber),
        containerPort: (.[2] | tonumber), proto: "tcp", count: 1
      })) as $ports |
      ([$current[0][] | select(.configuration.id != $name)] +
        [($previous[0][] | select(.configuration.id == $name)) //
          ($previous[0][0] | .configuration.id = $name)]) |
      map(if .configuration.id == $name then
        .configuration.labels = $labels | .configuration.publishedPorts = $ports |
        .configuration.image = {descriptor: {digest: (if $args[-4] | startswith("sha256:") then $args[-4] else $image end)}} | .status.state = "running"
      else . end)
    ' "${WRIX_TEST_RUN_ARGS}.nul" >"${WRIX_TEST_RUNTIME_JSON:?}.new"
    mv "${WRIX_TEST_RUNTIME_JSON}.new" "$WRIX_TEST_RUNTIME_JSON"
    exit 0
    ;;
  'exec '*)
    [[ "$#" -eq 6 && "$3" == wrix-service-supervisor && "$5" == /var/lib/wrix/service-supervisor ]]
    "${WRIX_TEST_JQ_BIN:?}" -e --arg name "$2" 'any(.[]; .configuration.id == $name and .status.state == "running")' "${WRIX_TEST_RUNTIME_JSON:?}" >/dev/null
    exec "${WRIX_TEST_COMPONENT_BIN:?}" "$4" "${WRIX_TEST_COMPONENT_ROOT:?}" "$6"
    ;;
  *) printf 'unexpected Apple container command: %s\n' "$*" >&2; exit 64 ;;
esac
cat "${WRIX_TEST_RUNTIME_JSON:?}"
