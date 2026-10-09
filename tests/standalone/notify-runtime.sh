#!/usr/bin/env bash
set -euo pipefail

wait_for_release() {
  local path="$1"
  local attempt
  for ((attempt = 0; attempt < 100; attempt += 1)); do
    if [[ -f "$path" ]]; then return 0; fi
    sleep 0.1
  done
  return 92
}

case "${1:-} ${2:-}" in
  'image inspect')
    if [[ "$(basename "$0")" == "container" ]]; then
      jq -cn --arg digest "${WRIX_TEST_DIGEST:?}" '[{id: $digest, digest: $digest}]'
    else
      printf '%s\n' "${WRIX_TEST_DIGEST:?}"
    fi
    ;;
  'images --format' | 'ps -a') ;;
  'image list' | 'list --all') printf '%s\n' '[]' ;;
  tag*) [[ "${2:-}" == "${WRIX_TEST_DIGEST:?}" ]] ;;
  'image tag') [[ "${3:-}" == "${WRIX_TEST_DIGEST:?}" ]] ;;
  'run --rm')
    shift 2
    pairs=()
    while [[ "$#" -gt 0 ]]; do
      case "$1" in
        -e) pairs+=("${2:?}"); shift 2 ;;
        *) shift ;;
      esac
    done
    jq -cn --args '$ARGS.positional' -- "${pairs[@]}" >"${WRIX_NOTIFY_TEST_ENV_CAPTURE:?}"
    shopt -s nullglob
    records=("${WRIX_NOTIFY_TEST_SESSION_DIR:?}"/*.json)
    if [[ "${#records[@]}" -eq 0 ]]; then
      printf '%s\n' 'null' >"${WRIX_TEST_CAPTURE:?}"
    elif [[ "${#records[@]}" -eq 1 ]]; then
      cp "${records[0]}" "${WRIX_TEST_CAPTURE:?}"
    else
      echo 'runtime fixture expected at most one registration' >&2
      exit 91
    fi
    if [[ -n "${WRIX_NOTIFY_TEST_READY:-}" ]]; then
      touch "$WRIX_NOTIFY_TEST_READY"
      wait_for_release "${WRIX_NOTIFY_TEST_RELEASE:?}"
    fi
    if [[ "${WRIX_NOTIFY_TEST_CLIENT:-0}" == "1" ]]; then
      # The external runtime boundary forwards only launcher env, not ambient host focus or tmux.
      env -u WRIX_FOCUS_TARGET -u WRIX_SESSION_ID -u TMUX "${pairs[@]}" \
        TMUX="in-container-debug-pane" PI_SESSION_ID="conversation:9.9" \
        WRIX_EXECUTION_ID="execution:9.9" WRIX_NOTIFY_TCP="${WRIX_NOTIFY_TEST_ENDPOINT:?}" \
        wrix-notify "${WRIX_NOTIFY_TEST_TITLE:?}" 'Waiting for input' 'Ping'
      if [[ -n "${WRIX_NOTIFY_TEST_FINISH:-}" ]]; then
        wait_for_release "$WRIX_NOTIFY_TEST_FINISH"
      fi
    fi
    printf '%s\n' '{"type":"response","command":"get_state","success":true,"data":{}}'
    ;;
  *) printf 'unexpected runtime arguments: %s\n' "$*" >&2; exit 91 ;;
esac
