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
  [[ "$(< "$configuration/version")" == 1 && ! -e "$runtime" && ! -L "$runtime" ]] || return 1
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
