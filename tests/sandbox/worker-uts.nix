{ wrix, linuxPkgs }:
wrix.mkSandbox {
  agent = "direct";
  agentPkg = linuxPkgs.writeShellScriptBin "worker-uts-probe" ''
    set -euo pipefail
    exec ${linuxPkgs.python3}/bin/python3 ${./worker-uts.py}
  '';
}
