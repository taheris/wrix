#!/bin/bash
set -euo pipefail
wrix_nix_client_phase=configuration

wrix_start_nix_client() {
  local configuration=/run/wrix/nix-pool
  local runtime=/run/wrix/nix-client
  local file relay attempt entries state_fd field value rest mount_id="" options
  if [[ ! -e "$configuration" && ! -L "$configuration" ]]; then
    return
  fi
  [[ -d "$configuration" && ! -L "$configuration" && "$("@STAT@" -c '%u:%g:%a' "$configuration")" == 0:0:700 ]] || return 1
  for file in key known-hosts ssh_config nix.conf version; do
    wrix_nix_client_phase="configuration-$file"
    [[ -f "$configuration/$file" && ! -L "$configuration/$file" && "$("@STAT@" -c '%u:%g:%a:%h' "$configuration/$file")" == 0:0:600:1 ]] || return 1
  done
  wrix_nix_client_phase="runtime-directory"
  [[ ! -e "$runtime" && ! -L "$runtime" ]] || return 1
  wrix_nix_client_phase="private-state"
  [[ -d /nix/var/nix && ! -L /nix/var/nix && ! -e /nix/var/nix/db && ! -L /nix/var/nix/db ]] || return 1
  wrix_nix_client_phase="readonly-state"
  if [[ $(< "$configuration/version") == 3 ]]; then
    wrix_nix_client_phase="readonly-state-preparation"
    [[ "$("@STAT@" -c '%u:%g:%a' /nix/var/nix)" == 0:0:755 ]] || return 1
    entries=$("@LS@" -A -- /nix/var/nix) || return 1
    [[ -z "$entries" ]] || return 1
    "@MOUNT@" --bind /nix/var/nix /nix/var/nix || return 1
    "@MOUNT@" -o remount,bind,ro,nodev,nosuid,noexec /nix/var/nix || return 1
  fi
  wrix_nix_client_phase="readonly-state"
  exec {state_fd}< /nix/var/nix || return 1
  while read -r field value rest; do
    if [[ "$field" == mnt_id: ]]; then
      [[ -z "$mount_id$rest" && "$value" =~ ^[1-9][0-9]*$ ]] || return 1
      mount_id="$value"
    fi
  done < "/proc/self/fdinfo/$state_fd"
  exec {state_fd}<&- || return 1
  [[ -n "$mount_id" ]] || return 1
  options=$("@FINDMNT@" --raw --noheadings --output VFS-OPTIONS --id "$mount_id") || return 1
  case ",$options," in
    *,ro,*) ;;
    *) return 1 ;;
  esac
  wrix_nix_client_phase="configuration-version"
  case "$(< "$configuration/version")" in
    1|3) ;;
    2)
      for ((attempt = 0; attempt < 300; attempt++)); do
        if [[ -S /run/wrix/apple-nix-bridge/socket ]]; then break; fi
        "@SLEEP@" 0.1
      done
      [[ -S /run/wrix/apple-nix-bridge/socket ]] || return 1
      "@RM@" -f /etc/nix/nix.conf
      "@LN@" -s "$configuration/nix.conf" /etc/nix/nix.conf
      ;;
    *) return 1 ;;
  esac
  wrix_nix_client_phase="relay-startup"
  "@MKDIR@" -m 0700 "$runtime"
  "@SOCAT@" "UNIX-LISTEN:$runtime/daemon.sock,mode=600,fork,max-children=16" "EXEC:@SSH@ -F $configuration/ssh_config nix-client@wrix-nix-pool nix-daemon --stdio" &
  relay=$!
  for ((attempt = 0; attempt < 100; attempt++)); do
    kill -0 "$relay" || return 1
    if [[ -S "$runtime/daemon.sock" ]]; then
      wrix_nix_client_phase="endpoint-readiness"
      export NIX_REMOTE="unix://$runtime/daemon.sock"
      "@TIMEOUT@" 15s "@NIX@" store info --json | "@JQ@" -e '.trusted == false' > /dev/null
      return
    fi
    "@SLEEP@" 0.1
  done
  return 1
}

wrix_start_nix_client || {
  printf 'wrix: fixed shared Nix client failed readiness; refusing agent startup (%s).\n' "$wrix_nix_client_phase" >&2
  exit 1
}
unset -f wrix_start_nix_client
unset wrix_nix_client_phase
