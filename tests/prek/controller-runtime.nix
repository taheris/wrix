{ pkgs }:
let
  backend =
    name:
    pkgs.writeScriptBin name ''
      #!${pkgs.python3}/bin/python3
      ${builtins.readFile ./backend-runtime.py}
    '';
  manager = pkgs.writeShellScriptBin "systemctl" ''
    set -euo pipefail
    if [[ -n "''${WRIX_TEST_CONTROLLER_MANAGER:-}" ]]; then
      exec "$WRIX_TEST_CONTROLLER_MANAGER" "$@"
    fi
    case "$*" in
      "--user show-environment"|"--user daemon-reload") ;;
      "--user start wrix-host-controller.service")
        [[ -S "''${WRIX_TEST_CONTROLLER_SOCKET:?fixture must own the live controller}" ]]
        ;;
      *)
        [[ "$#" -eq 3 && "$1" == --user && "$2" == link && -f "$3" ]]
        ;;
    esac
    printf '%s\n' "$*" >>"''${WRIX_TEST_CONTROLLER_CALLS:?}"
  '';
  retention = pkgs.writeShellScriptBin "nix-store" ''
    set -euo pipefail
    [[ "$#" -eq 5 && "$1" == --realise && "$3" == --add-root && "$5" == --indirect ]]
    [[ "$2" == /nix/store/* ]]
    [[ "$4" == /*/roots/* || "$4" == /*/maintenance-roots/* ]]
    ${pkgs.coreutils}/bin/ln -sfn -- "$2" "$4"
    printf '%s\n' "$*" >>"''${WRIX_TEST_CONTROLLER_CALLS:?}"
  '';
in
pkgs.symlinkJoin {
  name = "wrix-test-controller-runtime";
  paths = [
    manager
    retention
    (backend "container")
    (backend "podman")
  ];
}
