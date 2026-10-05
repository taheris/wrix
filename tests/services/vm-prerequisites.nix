{
  pkgs,
  system,
}:

let
  driver = pkgs.writeShellScriptBin "nixos-test-driver" ''
    set -euo pipefail
    [[ "$#" -eq 2 && "$1" == -o && -d "$2" && "$PWD" == "$2" ]]
    [[ "$PWD" != "$WRIX_TEST_VM_ROOT" ]]
    printf '%s\n' "$PWD" >>"$WRIX_TEST_VM_CALLS"
    case "$WRIX_TEST_VM_STATUS" in
      pass) touch "$2/guest-result" ;;
      fail) echo "fixture guest assertion: can't apply the child capabilities" >&2; exit 23 ;;
      skip) echo 'fixture guest exited 77' >&2; exit 77 ;;
      *) exit 64 ;;
    esac
  '';
  nix = pkgs.writeShellScriptBin "nix" ''
    set -euo pipefail
    [[ "$#" -eq 5 && "$1" == build && "$2" == --no-link && "$3" == --print-out-paths && "$4" == --no-warn-dirty ]]
    case "$5" in
      "$WRIX_TEST_VM_ROOT#legacyPackages.${system}.systemTests.services-devshell-start-independent.driver"|"$WRIX_TEST_VM_ROOT#legacyPackages.${system}.systemTests.services-limit-mode-cache-endpoint.driver") ;;
      *) exit 64 ;;
    esac
    printf '%s\n' "$5" >>"$WRIX_TEST_VM_BUILDS"
    if [[ "$WRIX_TEST_VM_BUILD_STATUS" != pass ]]; then
      echo "fixture driver build: can't apply the child capabilities" >&2
      exit "$WRIX_TEST_VM_BUILD_STATUS"
    fi
    printf '%s\n' ${driver}
  '';
  mkRunner =
    name: test:
    import ../lib/system-test.nix {
      pkgs = pkgs // {
        inherit nix;
      };
      inherit system name test;
    };
  lifecycle = mkRunner "test-services-devshell-start-independent" "services-devshell-start-independent";
  cache = mkRunner "test-services-limit-mode-cache-endpoint" "services-limit-mode-cache-endpoint";
in
pkgs.runCommandLocal "system-test-prerequisites"
  {
    nativeBuildInputs = [
      pkgs.bash
      pkgs.coreutils
      pkgs.git
      pkgs.jq
      pkgs.nix
      pkgs.python3
      pkgs.util-linux
    ];
  }
  ''
    set -euo pipefail
    python3 ${./test_vm_prerequisites.py} \
      ${lifecycle} ${cache} ${../lib/verifier.sh} ${system} ${pkgs.bash} \
      ${pkgs.util-linux}/bin/setpriv
    touch "$out"
  ''
