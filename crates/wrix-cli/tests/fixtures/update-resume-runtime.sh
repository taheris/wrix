#!/usr/bin/env bash
# jq variables must not expand in the shell.
# shellcheck disable=SC2016
set -euo pipefail
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
