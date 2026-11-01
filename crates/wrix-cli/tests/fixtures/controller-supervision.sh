#!/usr/bin/env bash
set -euo pipefail
root="$1"
pid="$2"
shift 2
case "$*" in
  print\ gui/*/org.wrix.host-controller)
    if [[ ! -f "$root/fixture-loaded" ]]; then
      echo 'Could not find service' >&2
      exit 113
    fi
    program="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["controller"] + "/bin/wrix-host-controller")' "$root/package.json")"
    printf 'path = %s/org.wrix.host-controller.plist\nprogram = %s\n' "$root" "$program"
    ;;
  print\ gui/*) [[ "$#" -eq 2 && "$2" != */org.wrix.host-controller ]] ;;
  bootstrap\ gui/*\ *)
    [[ "$#" -eq 3 && "$3" == "$root/org.wrix.host-controller.plist" && -f "$3" ]]
    [[ -S "$root/authority/control.sock" ]]
    kill -0 "$pid"
    touch "$root/fixture-loaded"
    ;;
  kickstart\ gui/*/org.wrix.host-controller)
    [[ -f "$root/fixture-loaded" && -S "$root/authority/control.sock" ]]
    kill -0 "$pid"
    ;;
  bootout\ gui/*/org.wrix.host-controller)
    kill -TERM "$pid"
    rm "$root/fixture-loaded"
    ;;
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
