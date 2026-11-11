{
  pkgs,
  casesFile,
  api,
}:
let
  tools = import ../../lib/verify/tools.nix { inherit pkgs; };
  permissions = pkgs.writeShellScript "check-emitted-nix-permissions" ''
    set -euo pipefail
    exec ${pkgs.python3}/bin/python3 ${./native-nix-permissions.py} "$@"
  '';
in
pkgs.runCommandLocal "shared-nix-image-contract" { } ''
  set -euo pipefail
  [[ -e ${api} ]]
  ${pkgs.lib.getExe tools} shared-nix-image ${casesFile} ${permissions}
  touch "$out"
''
