{ pkgs, wrix }:

let
  client = wrix.rustPackage.clientPackage;
  names = [
    "wrix"
    "wrix-cache-hook"
    "wrix-cache-publish"
    "wrix-focus-notification"
    "wrix-git-sign"
    "wrix-host-cache-policy"
  ];
  expected = pkgs.writeText "wrix-client-binaries" (builtins.concatStringsSep "\n" names + "\n");
in
pkgs.runCommand "wrix-client-package-boundary"
  {
    passthru = {
      inherit client;
      server = wrix.rustPackage.runtimePackage;
    };
  }
  ''
    set -euo pipefail
    find '${client}/bin' -type f -exec basename {} \; | sort > actual
    diff -u '${expected}' actual
    '${client}/bin/wrix' --help > help
    grep -F 'Usage: wrix' help
    touch "$out"
  ''
