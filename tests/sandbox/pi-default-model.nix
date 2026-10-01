{ pkgs, wrix }:
let
  pi = import ../../lib/sandbox/pi.nix { inherit pkgs; };
  inherit ((wrix.mkSandbox { agent = "pi"; })) image;
in
pkgs.runCommand "test-pi-default-model" { nativeBuildInputs = [ pkgs.nodejs ]; } ''
  node ${./pi-default-model.mjs} ${pi}/bin/pi ${image.piSettingsJson}
  touch "$out"
''
