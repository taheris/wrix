{ pkgs, seedRoots }:

if seedRoots == [ ] then
  throw "wrix-builder requires Darwin seed roots for its persistent Nix store"
else
  pkgs.writeShellScript "wrix-builder-store-export" ''
    set -euo pipefail

    mapfile -t closure < <(
      ${pkgs.nix}/bin/nix-store --query --requisites ${pkgs.lib.escapeShellArgs (map toString seedRoots)}
    )
    if [[ "''${#closure[@]}" -eq 0 ]]; then
      echo "Error: builder seed closure is empty" >&2
      exit 1
    fi
    exec ${pkgs.nix}/bin/nix-store --export "''${closure[@]}"
  ''
