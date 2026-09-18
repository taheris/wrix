#!/usr/bin/env bash
set -euo pipefail

# These guards also apply to Git hooks during sandbox bootstrap.
export BEADS_DOLT_AUTO_START=0
export BEADS_DOLT_SERVER_MODE=1
export BD_IMPORT_AUTO=false
export BD_EXPORT_AUTO=false
WRIX_BEADS_CONFIGURED=0

wrix_configure_beads_endpoint() {
  local workspace="$1"
  local backend
  [[ -f "$workspace/.beads/metadata.json" ]] || return 0
  backend=$(jq -er '.backend // "sqlite"' "$workspace/.beads/metadata.json") || {
    echo 'Error: invalid Beads metadata; refusing database fallback' >&2
    return 1
  }
  [[ "$backend" == "dolt" ]] || return 0
  if [[ -n "${BEADS_DOLT_SERVER_HOST:-}" || -n "${BEADS_DOLT_SERVER_PORT:-}" ]]; then
    if [[ -z "${BEADS_DOLT_SERVER_HOST:-}" || ! "${BEADS_DOLT_SERVER_PORT:-}" =~ ^[0-9]+$ ]]; then
      echo 'Error: incomplete Dolt TCP endpoint; the launcher must provide host and port' >&2
      return 1
    fi
    unset BEADS_DOLT_SERVER_SOCKET
  else
    export BEADS_DOLT_SERVER_SOCKET="${BEADS_DOLT_SERVER_SOCKET:-$workspace/.wrix/dolt.sock}"
    if [[ ! -S "$BEADS_DOLT_SERVER_SOCKET" ]]; then
      echo "Error: configured Dolt socket is unavailable: $BEADS_DOLT_SERVER_SOCKET" >&2
      return 1
    fi
    unset BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT
  fi
  WRIX_BEADS_CONFIGURED=1
}

wrix_wait_for_beads_endpoint() {
  [[ "$WRIX_BEADS_CONFIGURED" == "1" ]] || return 0
  local bd_bin="${WRIX_REAL_BD_BIN:-}"
  local deadline=$((SECONDS + 10))
  local output='No SQL response'
  local endpoint="${BEADS_DOLT_SERVER_SOCKET:-${BEADS_DOLT_SERVER_HOST:-}:${BEADS_DOLT_SERVER_PORT:-}}"
  if [[ -z "$bd_bin" ]]; then
    echo 'Error: bd is required to validate the workspace Dolt endpoint' >&2
    return 1
  fi
  while (( SECONDS < deadline )); do
    if output=$(timeout --signal=KILL 1 "$bd_bin" --readonly sql 'SELECT 1' 2>&1); then
      return 0
    fi
    sleep 0.2
  done
  printf 'Error: workspace Dolt endpoint %s is unreachable or rejected SQL from this sandbox.\n' "$endpoint" >&2
  printf 'Start/check the host service with wrix service start; refusing a competing Dolt server or JSONL import.\n%s\n' "$output" >&2
  return 1
}
