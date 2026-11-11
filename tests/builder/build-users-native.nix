{ pkgs }:
let
  image = import ../../lib/sandbox/builder/image.nix { inherit pkgs; };
  tools = import ../../lib/verify/tools.nix { inherit pkgs; };
  accountRoot =
    name: builtins.head (builtins.filter (root: root.name == name) image.darwin_seed_roots);
in
pkgs.runCommandLocal "test-builder-build-users-native"
  {
    nativeBuildInputs = [
      pkgs.bash
      pkgs.nix
    ];
  }
  ''
    set -euo pipefail
    mkdir -p "$TMPDIR/config"
    source ${../../lib/sandbox/builder/nix-daemon.sh}
    wrix_builder_write_nix_config builder "$TMPDIR/config/nix.conf"
    export NIX_CONF_DIR="$TMPDIR/config"
    export NIX_USER_CONF_FILES=/dev/null
    export NIX_CONFIG=""
    group=$(nix config show build-users-group)
    if [[ -z "$group" ]]; then
      echo "FAIL: builder Nix config runs builds as the root daemon rather than build users" >&2
      exit 1
    fi
    [[ "$(nix config show sandbox)" == false ]]
    trusted=$(nix config show trusted-users)
    ${pkgs.lib.getExe tools} builder-build-users \
      '${accountRoot "passwd"}/etc/passwd' '${accountRoot "group"}/etc/group' "$group" "$trusted"
    printf '%s\n' "PASS: builder config selects the image's unprivileged build-user pool"
    touch "$out"
  ''
