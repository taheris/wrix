#!/usr/bin/env bash
# jq variables must not expand in the shell.
# shellcheck disable=SC2016
set -euo pipefail
if [[ "${1:-}" == run && -f "${WRIX_TEST_FAIL_BACKEND_START:-}" ]]; then
  args=("$@")
  for ((i=0; i+1<${#args[@]}; i++)); do
    if [[ "${args[i]}" == --name && "${args[i+1]}" == "${WRIX_TEST_SERVICE_NAME:?}" ]]; then
      printf 'run\n' >>"$WRIX_TEST_FAIL_BACKEND_START"
      printf 'fixture backend creation interrupted\n' >&2
      exit 75
    fi
  done
fi
if [[ "$#" == 2 && ( "$1" == stop || "$1" == rm ) ]]; then
  printf '%s\n' "$*" >>"${WRIX_TEST_RUNTIME_LOG:?}"
  if [[ "$1" == stop ]]; then
    "${WRIX_TEST_JQ_BIN:?}" -e --arg name "$2" 'any(.[]; .configuration.id == $name)' "${WRIX_TEST_RUNTIME_JSON:?}" >/dev/null
    "$WRIX_TEST_JQ_BIN" --arg name "$2" 'map(if .configuration.id == $name then .status.state = "stopped" else . end)' "$WRIX_TEST_RUNTIME_JSON" >"$WRIX_TEST_RUNTIME_JSON.new"
  else
    "${WRIX_TEST_JQ_BIN:?}" -e --arg name "$2" 'any(.[]; .configuration.id == $name and .status.state == "stopped")' "${WRIX_TEST_RUNTIME_JSON:?}" >/dev/null
    "$WRIX_TEST_JQ_BIN" --arg name "$2" 'map(select(.configuration.id != $name))' "$WRIX_TEST_RUNTIME_JSON" >"$WRIX_TEST_RUNTIME_JSON.new"
  fi
  mv "$WRIX_TEST_RUNTIME_JSON.new" "$WRIX_TEST_RUNTIME_JSON"
  exit 0
fi
