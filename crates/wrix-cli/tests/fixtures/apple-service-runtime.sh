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
  'run -d --name '* | 'create --name '*)
    action=$1
    state=running
    [[ "$action" != create ]] || state=stopped
    if [[ "${!#}" == gateway ]]; then
      [[ "$action" == create ]] || native_gateway "$@"
      launch_args="${WRIX_TEST_RUN_ARGS:?}.gateway"
    else
      [[ "$action" == create ]] || printf '%s\n' "$@" >"${WRIX_TEST_RUN_ARGS:?}"
      launch_args="${WRIX_TEST_RUN_ARGS}.nul"
    fi
    printf '%s\0' "$@" >"$launch_args"
    "${WRIX_TEST_JQ_BIN:?}" --arg image "${WRIX_TEST_APPLE_CREATED_IMAGE:-sha256:${WRIX_TEST_DIGEST:?}}" --arg state "$state" -Rsc --slurpfile previous "${WRIX_TEST_RUNTIME_REPLACEMENT:?}" --slurpfile current "${WRIX_TEST_RUNTIME_JSON:?}" '
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
        .configuration.image = {descriptor: {digest: (if $args[-4] | startswith("sha256:") then $args[-4] else $image end)}} | .status.state = $state
      else . end)
    ' "$launch_args" >"${WRIX_TEST_RUNTIME_JSON:?}.new"
    mv "${WRIX_TEST_RUNTIME_JSON}.new" "$WRIX_TEST_RUNTIME_JSON"
    if [[ "$action" == create && "${!#}" == gateway ]]; then
      name=""
      for ((i = 1; i < $#; i++)); do
        if [[ "${!i}" == --name ]]; then
          next=$((i + 1))
          name=${!next}
          break
        fi
      done
      [[ -n "$name" ]]
      cp "$launch_args" "${WRIX_TEST_RUN_ARGS}.gateway.$name"
      printf '%s' "${WRIX_GATEWAY_KEY:?}" >"${WRIX_TEST_RUN_ARGS}.gateway.$name.key"
      chmod 600 "${WRIX_TEST_RUN_ARGS}.gateway.$name.key"
    fi
    exit 0
    ;;
  'start '*)
    [[ "$#" -eq 2 ]]
    "${WRIX_TEST_JQ_BIN:?}" -e --arg name "$2" 'any(.[]; .configuration.id == $name and .status.state == "stopped" and .configuration.labels["wrix.service.role"] == "gateway")' "${WRIX_TEST_RUNTIME_JSON:?}" >/dev/null
    args=()
    while IFS= read -r -d '' arg; do args+=("$arg"); done <"${WRIX_TEST_RUN_ARGS:?}.gateway.$2"
    WRIX_GATEWAY_KEY="$(<"${WRIX_TEST_RUN_ARGS}.gateway.$2.key")" native_gateway "${args[@]}"
    "$WRIX_TEST_JQ_BIN" --arg name "$2" 'map(if .configuration.id == $name then .status.state = "running" else . end)' "$WRIX_TEST_RUNTIME_JSON" >"${WRIX_TEST_RUNTIME_JSON}.new"
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
