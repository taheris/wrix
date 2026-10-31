#!/usr/bin/env bash
set -euo pipefail
root="$1"
pidfile="$2"
shift 2
alive() {
  local pid state rest
  if ! read -r pid <"$pidfile"; then return 1; fi
  if [[ ! -r "/proc/$pid/stat" ]]; then return 1; fi
  if ! read -r rest rest state rest <"/proc/$pid/stat"; then return 1; fi
  [[ "$state" != Z ]] && kill -0 "$pid"
}
case "$*" in
  '--user show-environment'|'--user daemon-reload') ;;
  '--user start wrix-host-controller.service')
    if ! alive; then : >"$root/restart.request"; fi
    for ((attempt=0; attempt<1000; attempt++)); do
      if alive && [[ -S "$root/authority/control.sock" ]]; then exit 0; fi
      sleep 0.02
    done
    echo 'fixture controller restart deadline exceeded' >&2
    exit 1
    ;;
  '--user stop wrix-host-controller.service')
    if alive; then
      read -r pid <"$pidfile"
      kill -TERM "$pid"
      for ((attempt=0; attempt<1000; attempt++)); do
        if ! alive; then exit 0; fi
        sleep 0.02
      done
      echo 'fixture controller stop deadline exceeded' >&2
      exit 1
    fi
    ;;
  '--user show --property=LoadState,ActiveState,FragmentPath wrix-host-controller.service')
    [[ -f "$root/wrix-host-controller.service" ]]
    state=inactive
    if alive; then state=active; fi
    printf 'LoadState=loaded\nActiveState=%s\nFragmentPath=%s/wrix-host-controller.service\n' "$state" "$root"
    ;;
  *) [[ "$#" -eq 3 && "$1" == --user && "$2" == link && "$3" == "$root/wrix-host-controller.service" && -f "$3" ]] ;;
esac
