{ pkgs }:
let
  tools = import ../../lib/verify/native-test.nix { inherit pkgs; };
in
pkgs.writeShellScript "native-fixture-cleanup" ''
  set -euo pipefail
  exec ${tools}/bin/wrix-native-test native-fixture-cleanup "$@"
''
