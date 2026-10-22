#!/usr/bin/env bash
set -euo pipefail

_wrix_cli_error() {
  printf '%s\n' "wrix: $*" >&2
  printf '%s\n' \
    "Provide an updated host Wrix using Home Manager, nix-darwin, or:" \
    "  nix profile add 'github:taheris/wrix#wrix'" \
    "For temporary provision: nix shell 'github:taheris/wrix#wrix', then re-enter the project shell." >&2
}

_wrix_select_cli() {
  local selected help
  selected="${WRIX_BIN:-wrix}"
  if ! selected="$(type -P -- "$selected")" || [[ ! -x "$selected" ]]; then
    _wrix_cli_error "selected executable ${WRIX_BIN:-wrix} is unavailable; no Wrix activation effects were performed."
    return 1
  fi
  if [[ "$selected" != /* ]]; then
    selected="$PWD/$selected"
  fi
  if [[ -n "${WRIX_PROJECT_PROFILE_CONFIG:-}" ]]; then
    if ! help="$("$selected" run --help)" || [[ "$help" != *'generated project'* && "$help" != *'generated immutable project'* ]]; then
      _wrix_cli_error "selected executable $selected does not support generated project launch data; no Wrix activation effects were performed."
      return 1
    fi
    if ! help="$("$selected" service capabilities 2>/dev/null)" || [[ "$help" != shared-services-v1 ]]; then
      _wrix_cli_error "selected executable $selected lacks the shared-service protocol; update the installed host CLI and clear or update any stale WRIX_BIN before re-entering this shell. No Wrix activation effects were performed."
      return 1
    fi
  fi
  printf '%s\n' "$selected"
}

_wrix_select_cli
unset -f _wrix_select_cli _wrix_cli_error
