{ pkgs }:

let
  inherit (pkgs) writeShellScriptBin symlinkJoin;
  attach = writeShellScriptBin "wrix-nix-pool-attach" ''
    set -euo pipefail
    umask 077
    [[ "$#" == 3 && "$1" == /nix/store/*/bin/container && "$2" =~ ^wrix-nix-[a-f0-9]{32}$ ]]
    container="$1"
    name="$2"
    receipt="$3"
    [[ "$receipt" == /* && ! -e "$receipt" && ! -L "$receipt" ]]
    if ${pkgs.coreutils}/bin/env -i HOME="$HOME" LC_ALL=C PATH=/usr/bin:/bin "$container" start --attach "$name"; then
      status=0
    else
      status=$?
    fi
    temporary="$receipt.pending"
    (set -o noclobber; printf '%s\n' "$status" > "$temporary")
    ${pkgs.coreutils}/bin/sync "$temporary"
    ${pkgs.coreutils}/bin/ln "$temporary" "$receipt"
    ${pkgs.coreutils}/bin/sync "$receipt" "$(${pkgs.coreutils}/bin/dirname "$receipt")"
    ${pkgs.coreutils}/bin/rm "$temporary"
    exit "$status"
  '';
  unmount = writeShellScriptBin "wrix-nix-pool-unmount" ''
    set -euo pipefail
    [[ "$#" == 1 && "$1" == /* ]]
    exec /sbin/umount "$1"
  '';
  ifconfig = writeShellScriptBin "wrix-nix-pool-ifconfig" ''
    set -euo pipefail
    [[ "$#" == 0 ]]
    exec /sbin/ifconfig -a
  '';
  uname = writeShellScriptBin "wrix-nix-pool-uname" ''
    set -euo pipefail
    [[ "$#" == 0 ]]
    exec /usr/bin/uname -r
  '';
  launchctl = writeShellScriptBin "wrix-nix-pool-launchctl" ''
    set -euo pipefail
    exec /bin/launchctl "$@"
  '';
in
symlinkJoin {
  name = "wrix-nix-pool-apple-host";
  paths = [
    pkgs.container
    pkgs.sshfs-fuse
    attach
    ifconfig
    uname
    launchctl
    unmount
  ];
}
