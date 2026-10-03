{ pkgs }:

let
  missingPrek = pkgs.runCommand "wrix-test-missing-prek" { } ''
    set -euo pipefail
    mkdir -p "$out/bin"
  '';
in
import ../../lib/prek/runner.nix {
  pkgs = pkgs // {
    prek = missingPrek;
  };
}
