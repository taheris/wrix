{ pkgs }:
let
  closure = pkgs.closureInfo { rootPaths = [ pkgs.bash ]; };
in
pkgs.writeShellApplication {
  name = "wrix-test-host-hook-store-init";
  runtimeInputs = [
    pkgs.coreutils
    pkgs.nix
  ];
  text = ''
    [[ "$#" -eq 1 ]]
    store="$1"
    [[ "$store" == /* && ! -e "$store" ]]
    mkdir -p "$store/nix/store"
    while IFS= read -r path; do
      [[ "$path" == /nix/store/* ]]
      nix-store --dump "$path" > "$store/fixture.nar"
      nix-store --restore "$store$path" < "$store/fixture.nar" > /dev/null
      rm "$store/fixture.nar"
    done < ${closure}/store-paths
    nix-store --store "$store" --load-db < ${closure}/registration > /dev/null
    while IFS= read -r path; do
      nix-store --store "$store" --verify-path "$path" > /dev/null
    done < ${closure}/store-paths
    printf '%s\n' '${pkgs.bash}/bin/bash'
  '';
}
