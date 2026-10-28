{ pkgs }:
let
  owner = pkgs.writeShellScriptBin "wrix-nix-pool-owner" ''
    set -euo pipefail
    printf '%s\0' "$@" > "$delegated"
    exit 23
  '';
  observer = import ./native-observer-owner.nix { inherit pkgs owner; };
in
pkgs.runCommand "native-observer-owner-delegation" { } ''
  set -euo pipefail
  export delegated="$TMPDIR/delegated"
  test "$EUID" != 0
  check_delegation() {
    local status=0
    ${observer}/bin/wrix-nix-pool-owner "$@" || status=$?
    test "$status" == 23
    printf '%s\0' "$@" > "$TMPDIR/expected"
    cmp "$TMPDIR/expected" "$delegated"
  }
  for request in keys control bootstrap 'argument with spaces'; do
    check_delegation "$request"
  done
  for observation in boot-id serve snapshot stop kill-daemon; do
    export SSH_ORIGINAL_COMMAND="wrix-native-observe-$observation"
    check_delegation control
    check_delegation keys
  done
  export SSH_ORIGINAL_COMMAND='wrix-native-observe-snapshot; id'
  check_delegation control
  check_delegation control extra
  WRIX_TEST_NATIVE_BOUNDARY_SCRIPT=${./native-boundary.py} \
    ${pkgs.python3}/bin/python3 ${./test_native_boundary.py}
  touch "$out"
''
