{ wrix, linuxPkgs }:
let
  native = import ../../lib/verify/native-test.nix { pkgs = linuxPkgs; };
in
wrix.mkSandbox {
  agent = "direct";
  agentPkg = linuxPkgs.writeShellScriptBin "worker-uts-probe" ''
    set -euo pipefail
    exec ${native}/bin/wrix-worker-uts --unshare ${linuxPkgs.util-linux}/bin/unshare
  '';
}
