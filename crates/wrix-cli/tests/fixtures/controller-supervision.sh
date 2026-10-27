#!/usr/bin/env bash
set -euo pipefail
root="$1"
pid="$2"
shift 2
case "$*" in
  '--user show-environment'|'--user daemon-reload') ;;
  '--user start wrix-host-controller.service')
    [[ -S "$root/authority/control.sock" ]]
    kill -0 "$pid"
    ;;
  '--user stop wrix-host-controller.service') kill -TERM "$pid" ;;
  '--user show --property=LoadState,ActiveState,FragmentPath wrix-host-controller.service')
    [[ -f "$root/wrix-host-controller.service" ]]
    printf 'LoadState=loaded\nActiveState=active\nFragmentPath=%s/wrix-host-controller.service\n' "$root"
    ;;
  *) [[ "$#" -eq 3 && "$1" == --user && "$2" == link && "$3" == "$root/wrix-host-controller.service" && -f "$3" ]] ;;
esac
