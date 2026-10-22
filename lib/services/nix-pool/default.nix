{ pkgs }:

let
  inherit (builtins) concatStringsSep genList toString;
  inherit (pkgs)
    runCommand
    writeText
    writeShellScript
    writeShellScriptBin
    ;
  users = genList (index: {
    name = "nixbld${toString (index + 1)}";
    uid = 30001 + index;
  }) 32;
  passwd = writeText "nix-pool-passwd" (
    "root:x:0:0:Pool administrator:/var/empty:/bin/false\n"
    + "nix-client:x:2000:2000:Native Nix client:/var/empty:/bin/false\n"
    + concatStringsSep "" (
      map (user: "${user.name}:x:${toString user.uid}:30000:Nix build user:/var/empty:/bin/false\n") users
    )
  );
  group = writeText "nix-pool-group" (
    "root:x:0:\nnix-client:x:2000:\n"
    + "nixbld:x:30000:${concatStringsSep "," (map (user: user.name) users)}\n"
  );
  nixConf = writeText "nix-pool.conf" ''
    experimental-features = nix-command
    sandbox = true
    sandbox-fallback = false
    build-users-group = nixbld
    allowed-users = root nix-client
    trusted-users = root
    require-sigs = true
    substituters = https://cache.nixos.org
    builders =
    max-jobs = 2
    cores = 2
    min-free = 0
    max-free = 0
  '';
  filesystem = runCommand "nix-pool-filesystem" { } ''
    mkdir -p "$out/etc/nix" "$out/etc/wrix"
    cp ${passwd} "$out/etc/passwd"
    cp ${group} "$out/etc/group"
    cp ${nixConf} "$out/etc/nix/nix.conf"
    printf 'passwd: files\ngroup: files\nhosts: files dns\n' > "$out/etc/nsswitch.conf"
    printf '1\n' > "$out/etc/wrix/nix-pool-service"
  '';
  runtime = pkgs.symlinkJoin {
    name = "nix-pool-runtime";
    paths = [
      pkgs.bash
      pkgs.coreutils
      pkgs.nix
      pkgs.util-linux
      pkgs.cacert
    ];
  };
  seedClosure = pkgs.closureInfo {
    rootPaths = [
      runtime
      filesystem
    ];
  };
  initialize = writeShellScript "nix-pool-initialize" ''
    set -euo pipefail
    umask 077

    export PATH=${runtime}/bin:/bin:/usr/bin
    root=/var/lib/wrix/nix-pool
    [[ $(id -u) == 0 && ! -e /etc/wrix/image-agent && -f /etc/wrix/nix-pool-service ]]
    [[ $(</etc/wrix/nix-pool-service) == 1 && $(readlink -e "$root") == "$root" ]]
    [[ $(stat -c '%u:%g:%a' "$root") == 0:0:700 ]]
    export HOME=/var/empty
    export NIX_CONF_DIR=${filesystem}/etc/nix
    export NIX_USER_CONF_FILES=/dev/null
    export NIX_STORE_DIR=/nix/store
    export NIX_STATE_DIR=/nix/var/nix
    export NIX_DAEMON_SOCKET_PATH=/nix/var/nix/daemon-socket/socket
    export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt

    shopt -s nullglob dotglob
    for entry in "$root"/*; do
      if [[ "$entry" != "$root/ownership.lock" ]]; then
        printf 'Retained Nix state or uncertain ownership requires explicit host reconciliation.\n' >&2
        exit 73
      fi
    done
    if ! mkdir -m 0700 "$root/owner-active"; then
      printf 'Nix pool ownership is active or uncertain; explicit host reconciliation is required.\n' >&2
      exit 73
    fi
    printf '1:%s\n' '${pkgs.nix.version}' > "$root/owner-active/version"
    sync "$root/owner-active/version" "$root/owner-active" "$root"

    install -d -m 0755 "$root/nix" "$root/nix/var" "$root/nix/var/nix"
    install -d -m 1775 -o 0 -g 30000 "$root/nix/store"
    mapfile -t roots < ${seedClosure}/store-paths
    entrypoint=$(readlink -e /bin/wrix-nix-pool)
    [[ "$entrypoint" == /nix/store/*/bin/wrix-nix-pool ]]
    tail=''${entrypoint#/nix/store/}
    bootstrap_root=/nix/store/''${tail%%/*}
    roots+=("$bootstrap_root")
    nix copy --no-check-sigs --from local --to "local?root=$root" "''${roots[@]}"
    install -d -m 0755 "$root/nix/var/nix/gcroots/bootstrap" "$root/nix/var/nix/daemon-socket"
    for path in "''${roots[@]}"; do
      ln -s "$path" "$root/nix/var/nix/gcroots/bootstrap/''${path##*/}"
    done
    sync "$root/nix/var/nix/gcroots/bootstrap" "$root/nix/var/nix"
    mount --bind "$root/nix" /nix
    exec nix-daemon --store local
  '';
  bootstrap = writeShellScriptBin "wrix-nix-pool" ''
    set -euo pipefail
    umask 077

    export PATH=${runtime}/bin:/bin:/usr/bin
    if [[ "$#" != 0 || "$(id -u)" != 0 || -e /etc/wrix/image-agent ]]; then
      printf 'Nix pool bootstrap is a guest-service operation without caller arguments.\n' >&2
      exit 64
    fi
    [[ -f /etc/wrix/nix-pool-service && ! -L /var/lib/wrix/nix-pool ]]
    [[ $(</etc/wrix/nix-pool-service) == 1 ]]
    root=/var/lib/wrix/nix-pool
    [[ $(readlink -e "$root") == "$root" && $(stat -c '%u:%g:%a' "$root") == 0:0:700 ]]
    lock="$root/ownership.lock"
    if [[ ! -e "$lock" && ! -L "$lock" ]]; then
      (set -o noclobber; : > "$lock")
    fi
    [[ -f "$lock" && ! -L "$lock" && $(stat -c '%u:%g:%a:%h' "$lock") == 0:0:600:1 ]]
    exec env -i HOME=/var/empty PATH=${runtime}/bin:/bin:/usr/bin \
      ${runtime}/bin/flock --exclusive --nonblock --conflict-exit-code 73 "$lock" ${initialize}
  '';
in
{
  inherit
    runtime
    filesystem
    bootstrap
    seedClosure
    ;
  imageCommands = ''
    mkdir -p var/empty var/lib/wrix/nix-pool
    chmod 0700 var/lib/wrix/nix-pool
    chmod 0755 nix/var/nix nix/var/nix/db
  '';
}
