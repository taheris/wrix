#!/bin/bash
set -euo pipefail

wrix_start_nix_client() {
  local configuration=/run/wrix/nix-pool
  local runtime=/run/wrix/nix-client
  local file relay attempt
  if [[ ! -e "$configuration" && ! -L "$configuration" ]]; then
    return
  fi
  [[ -d "$configuration" && ! -L "$configuration" && "$("@STAT@" -c '%u:%g:%a' "$configuration")" == 0:0:700 ]] || return 1
  for file in key known-hosts ssh_config nix.conf version; do
    [[ -f "$configuration/$file" && ! -L "$configuration/$file" && "$("@STAT@" -c '%u:%g:%a:%h' "$configuration/$file")" == 0:0:600:1 ]] || return 1
  done
  [[ ! -e "$runtime" && ! -L "$runtime" ]] || return 1
  [[ -d /nix/var/nix && ! -L /nix/var/nix && ! -e /nix/var/nix/db && ! -L /nix/var/nix/db ]] || return 1
  case ",$("@FINDMNT@" -n -o VFS-OPTIONS --mountpoint /nix/var/nix)," in
    *,ro,*) ;;
    *) return 1 ;;
  esac
  case "$(< "$configuration/version")" in
    1) ;;
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
  "@MKDIR@" -m 0700 "$runtime"
  "@SOCAT@" "UNIX-LISTEN:$runtime/daemon.sock,mode=600,fork,max-children=16" "EXEC:@SSH@ -F $configuration/ssh_config nix-client@wrix-nix-pool nix-daemon --stdio" &
  relay=$!
  for ((attempt = 0; attempt < 100; attempt++)); do
    kill -0 "$relay" || return 1
    if [[ -S "$runtime/daemon.sock" ]]; then
      export NIX_REMOTE="unix://$runtime/daemon.sock"
      "@TIMEOUT@" 15s "@NIX@" store info --json | "@JQ@" -e '.trusted == false' > /dev/null
      return
    fi
    "@SLEEP@" 0.1
  done
  return 1
}

wrix_start_nix_client || {
  printf 'wrix: fixed shared Nix client failed readiness; refusing agent startup.\n' >&2
  exit 1
}
unset -f wrix_start_nix_client
