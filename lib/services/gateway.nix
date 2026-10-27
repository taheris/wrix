{ pkgs, rustPackage }:
pkgs.runCommand "wrix-egress-gateway" { } ''
  set -euo pipefail
  install -Dm755 "${rustPackage}/bin/wrix-egress-gateway" "$out/bin/wrix-egress-gateway"
''
