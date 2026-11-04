{
  pkgs,
  initializer ? ../../lib/sandbox/initialize-private-nix.sh,
}:
let
  # Use the actual stable-tier producer without realizing its image or Linux tools.
  inherit
    (
      (import ../../lib/sandbox/stable-profile-image.nix {
        inherit pkgs;
        profile.name = "gc-fixture";
      })
    )
    configuration
    ;
  roots = builtins.attrValues configuration;
  seed = pkgs.closureInfo { rootPaths = roots; };
in
pkgs.runCommand "test-private-nix-image-roots"
  {
    nativeBuildInputs = [
      pkgs.bash
      pkgs.coreutils
      pkgs.fakeroot
      pkgs.lsof
      pkgs.nix
      pkgs.python3
    ]
    ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.stdenv.cc ];
  }
  ''
    set -euo pipefail
    root="$TMPDIR/image-root"
    mkdir -p "$root/etc/wrix" "$root/etc/nix"
    printf '1\n' > "$root/etc/wrix/private-nix-version"
    printf '%s\n' ${pkgs.lib.escapeShellArgs (map toString roots)} > "$root/etc/wrix/store-roots"
    ln -s ${configuration.passwd}/etc/passwd "$root/etc/passwd"
    ln -s ${configuration.group}/etc/group "$root/etc/group"
    ln -s ${configuration.nix}/etc/nix/nix.conf "$root/etc/nix/nix.conf"

    # This URI confines import, registration and real GC to the disposable store.
    store="local?root=$root"
    mkdir -p "$root/nix/store"
    for path in ${pkgs.lib.escapeShellArgs (map toString roots)}; do
      cp -r "$path" "$root$path"
    done
    nix-store --store "$store" --load-db < ${seed}/registration
    printf 'collectible unrelated garbage\n' > "$TMPDIR/unrelated-garbage"
    garbage=$(nix-store --store "$store" --add "$TMPDIR/unrelated-garbage")
    test -e "$root$garbage"
    fakeroot -- bash ${initializer} "$root"
    nix-store --store "$store" --gc
    test ! -e "$root$garbage"
    nix-store --store "$store" --verify --check-contents

    # Resolve through the image's logical root, never ambient /nix/store, which
    # could mask collection of an isolated-store path with a host-store copy.
    passwd="$root$(readlink "$root/etc/passwd")"
    group="$root$(readlink "$root/etc/group")"
    config="$root$(readlink "$root/etc/nix/nix.conf")"
    cmp ${configuration.passwd}/etc/passwd "$passwd"
    cmp ${configuration.group}/etc/group "$group"
    cmp ${configuration.nix}/etc/nix/nix.conf "$config"
    ${pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
      $CC -Wall -Wextra -Werror ${./image-gc-accounts.c} -o "$TMPDIR/account-lookup"
      "$TMPDIR/account-lookup" "$passwd" "$group"
    ''}
    ${pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
      echo 'Darwin: account-file bytes retained; native Linux libc lookup requires the Linux check.'
    ''}
    NIX_CONFIG= NIX_USER_CONF_FILES=/dev/null NIX_CONF_DIR="$(dirname "$config")" \
      nix --store "$store" config show --json > "$TMPDIR/config.json"
    python3 - "$TMPDIR/config.json" <<'PY'
    import json
    import sys
    with open(sys.argv[1]) as source:
        config = json.load(source)
    assert config["build-users-group"]["value"] == ""
    assert config["sandbox"]["value"] is False
    assert {"flakes", "nix-command"} <= set(config["experimental-features"]["value"])
    PY
    touch "$out"
  ''
