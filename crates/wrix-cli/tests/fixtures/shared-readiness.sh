#!/usr/bin/env bash
# jq variables and the literal external SQL command must not expand in this shell.
# shellcheck disable=SC2016
# Prefix for the cache-only external runtime fixture. This models just the
# native management readiness query; namespace SQL is never emulated here.
set -euo pipefail
if [[ "${1:-}" == start && -n "${WRIX_TEST_RUNTIME_JSON:-}" ]]; then
  [[ "$#" == 2 ]]
  printf '%s\n' "$*" >>"${WRIX_TEST_RUNTIME_LOG:?}"
  "${WRIX_TEST_JQ_BIN:?}" -e --arg name "$2" 'any(.[]; .configuration.id == $name and .status.state == "stopped" and .configuration.labels["wrix.dolt.shared"] == "native-v1")' "$WRIX_TEST_RUNTIME_JSON" >/dev/null
  "$WRIX_TEST_JQ_BIN" --arg name "$2" 'map(if .configuration.id == $name then .status.state = "running" else . end)' "$WRIX_TEST_RUNTIME_JSON" >"${WRIX_TEST_RUNTIME_JSON}.new"
  mv "${WRIX_TEST_RUNTIME_JSON}.new" "$WRIX_TEST_RUNTIME_JSON"
  # Restart the external cache daemon from the retained original launch recipe.
  tr '\0' '\n' <"${WRIX_TEST_RUN_ARGS:?}.nul" >"$WRIX_TEST_RUN_ARGS"
  exit 0
fi
if [[ "${1:-}" == exec && "${3:-}" == sh ]]; then
  if [[ -n "${WRIX_TEST_RUNTIME_JSON:-}" ]]; then
    printf '%s\n' "$*" >>"${WRIX_TEST_RUNTIME_LOG:?}"
    probe_args="${WRIX_TEST_RUN_ARGS:?}.nul"
  else
    printf '%s\0' "$@" >>"${WRIX_TEST_CALLS:?}"
    probe_args="${WRIX_TEST_RUN_ARGS:?}"
  fi
  expected='export DOLT_CLI_PASSWORD="$(cat -- "$1/admin.password")"; exec dolt --host 127.0.0.1 --port 3306 --no-tls --password "$DOLT_CLI_PASSWORD" --user wrixadmin --use-db controller sql -r json -q '\''SELECT @@GLOBAL.secure_file_priv AS secure_file_priv'\'''
  if [[ "$#" != 7 || "$3" != sh || "$4" != -c || "$5" != "$expected" || "$6" != wrix-ready ]]; then
    printf 'unsupported native management probe\n' >&2
    exit 64
  fi
  "${WRIX_TEST_JQ_BIN:?}" -e --arg name "$2" --arg data "$7" -Rsc '
    split("\u0000")[:-1] as $args |
    def values($flag): [range(0; ($args | length) - 1) as $i |
      select($args[$i] == $flag) | $args[$i + 1]];
    (values("--name") == [$name]) and
    (values("--label") | index("wrix.dolt.shared=native-v1") != null) and
    (values("-v") | index($data + ":" + $data + ":rw") != null)
  ' "$probe_args" >/dev/null
  if [[ -n "${WRIX_TEST_RUNTIME_JSON:-}" ]]; then
    "${WRIX_TEST_JQ_BIN:?}" -e --arg name "$2" 'any(.[]; .configuration.id == $name and .status.state == "running")' "$WRIX_TEST_RUNTIME_JSON" >/dev/null
  else
    [[ -f "${WRIX_TEST_RUNNING:?}" ]]
  fi
  printf '{"rows":[{"secure_file_priv":"/dev/null"}]}\n'
  exit 0
fi
