{
  pkgs,
  casesFile,
  api,
}:
let
  inherit (pkgs.lib) getExe;
  tools = import ../../lib/verify/tools.nix { inherit pkgs; };
  permissions = pkgs.writeShellScript "check-emitted-nix-permissions" ''
    set -euo pipefail
    exec ${getExe tools} native-nix-permissions "$@"
  '';
in
pkgs.runCommandLocal "shared-nix-image-contract" { } ''
  set -euo pipefail
  [[ -e ${api} ]]
  ${getExe tools} shared-nix-image ${casesFile} ${permissions}
  touch "$out"
''
