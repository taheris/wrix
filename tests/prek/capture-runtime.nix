{ pkgs }:

let
  inherit (pkgs) prek writeShellScriptBin;

  capturePrek = writeShellScriptBin "prek" ''
    set -euo pipefail
    printf '%s\0' "$@" >>"''${WRIX_TEST_PREK_ARGV:?}"
    cat >"''${WRIX_TEST_PREK_STDIN:?}"
    exec ${prek}/bin/prek "$@" <"$WRIX_TEST_PREK_STDIN"
  '';
in
import ../../lib/prek/runner.nix {
  pkgs = pkgs // {
    prek = capturePrek;
  };
}
