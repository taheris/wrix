{ pkgs, owner }:

let
  inherit (pkgs)
    runCommand
    writeText
    writeShellScript
    writeShellScriptBin
    ;
  network = import ./network.nix { inherit pkgs; };
  resources = import ./resources.nix { inherit pkgs owner sshdConf; };
  sshdConf = writeText "nix-pool-sshd.conf" ''
    Port 2222
    ListenAddress 0.0.0.0
    HostKey /var/lib/wrix/nix-pool/host-key
    PidFile /run/wrix-nix-pool/sshd.pid
    AuthorizedKeysFile none
    AuthorizedKeysCommand /run/wrix-nix-pool/keys keys %u
    AuthorizedKeysCommandUser root
    AuthenticationMethods publickey
    PasswordAuthentication no
    KbdInteractiveAuthentication no
    PermitRootLogin forced-commands-only
    AllowUsers root nix-client nix-export
    PermitUserEnvironment no
    AllowAgentForwarding no
    AllowTcpForwarding no
    PermitTunnel no
    X11Forwarding no
    PermitTTY no
    MaxAuthTries 2
    MaxSessions 1
    MaxStartups 16:30:32
    UsePAM no
    Subsystem sftp internal-sftp
    Match User nix-export
      ChrootDirectory /var/lib/wrix/nix-pool/export-root
  '';
  passwd = writeText "nix-pool-passwd" ''
    root:x:0:0:Pool administrator:/var/empty:/bin/sh
    nix-client:x:2000:2000:Native Nix client:/var/empty:/bin/sh
    nix-export:x:2001:2001:Read-only Nix filesystem:/var/empty:/bin/sh
    sshd:x:74:74:SSH privilege separation:/var/empty:/bin/false
    nixbld1:x:30001:30000:Nix build user:/var/empty:/bin/false
  '';
  group = writeText "nix-pool-group" ''
    root:x:0:
    nix-client:x:2000:
    nix-export:x:2001:
    sshd:x:74:
    nixbld:x:30000:nixbld1
  '';
  nixConf = writeText "nix-pool.conf" ''
    experimental-features = nix-command cgroups daemon-trust-override
    use-cgroups = true
    sandbox = true
    sandbox-fallback = false
    build-users-group = nixbld
    allowed-users = root nix-client
    trusted-users = root
    require-sigs = true
    substituters = https://cache.nixos.org
    builders =
    max-jobs = 1
    cores = 2
    min-free = 0
    max-free = 0
    build-dir = /nix/var/nix/builds
    max-build-log-size = 67108864
  '';
  filesystem = runCommand "nix-pool-filesystem" { } ''
    mkdir -p "$out/etc/nix" "$out/etc/wrix"
    cp ${passwd} "$out/etc/passwd"
    cp ${group} "$out/etc/group"
    cp ${nixConf} "$out/etc/nix/nix.conf"
    printf 'nameserver 1.1.1.1\nnameserver 9.9.9.9\noptions timeout:2 attempts:2\n' > "$out/etc/resolv.conf"
    cp ${
      writeText "nix-pool-native.json" (
        builtins.toJSON {
          nix_daemon = "${pkgs.nix}/bin/nix-daemon";
          nix_store = "${pkgs.nix}/bin/nix-store";
          sftp_server = "${pkgs.openssh}/libexec/sftp-server";
          helper = "${owner}/bin/wrix-nix-pool-owner";
        }
      )
    } "$out/etc/wrix/nix-pool-native.json"
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
      pkgs.openssh
      owner
      (pkgs.lib.getBin pkgs.e2fsprogs)
      network.install
      resources
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
      if [[ "$entry" != "$root/ownership.lock" && "$entry" != "$root/bootstrap.json" ]]; then
        printf 'Retained Nix state or uncertain ownership requires explicit host reconciliation.\n' >&2
        exit 73
      fi
    done
    ${owner}/bin/wrix-nix-pool-owner keys root > /dev/null
    if ! mkdir -m 0700 "$root/owner-active"; then
      printf 'Nix pool ownership is active or uncertain; explicit host reconciliation is required.\n' >&2
      exit 73
    fi
    ${network.install}/bin/wrix-nix-pool-egress
    printf '2:%s:8589934592\n' '${pkgs.nix.version}' > "$root/owner-active/version"
    sync "$root/owner-active/version" "$root/owner-active" "$root"

    fallocate --length=8589934592 "$root/store.ext4"
    mkfs.ext4 -q -F -m 0 "$root/store.ext4"
    install -d -m 0700 "$root/filesystem"
    device=$(losetup --find --show "$root/store.ext4")
    mount -t ext4 -o nodev,nosuid "$device" "$root/filesystem"
    storage="$root/filesystem"
    install -d -m 0700 "$root/leases" "$root/incoming"
    ssh-keygen -q -t ed25519 -N "" -C "" -f "$root/host-key"
    install -d -m 0755 "$storage/nix" "$storage/nix/var" "$storage/nix/var/nix"
    install -d -m 1775 -o 0 -g 30000 "$storage/nix/store"
    mapfile -t roots < ${seedClosure}/store-paths
    entrypoint=$(readlink -e /bin/wrix-nix-pool)
    [[ "$entrypoint" == /nix/store/*/bin/wrix-nix-pool ]]
    tail=''${entrypoint#/nix/store/}
    bootstrap_root=/nix/store/''${tail%%/*}
    roots+=("$bootstrap_root")
    nix copy --no-check-sigs --from local --to "local?root=$storage" "''${roots[@]}"
    install -d -m 0755 "$storage/nix/var/nix/gcroots/bootstrap" "$storage/nix/var/nix/daemon-socket" "$storage/nix/var/nix/builds"
    for path in "''${roots[@]}"; do
      ln -s "$path" "$storage/nix/var/nix/gcroots/bootstrap/''${path##*/}"
    done
    sync "$storage/nix/var/nix/gcroots/bootstrap" "$storage/nix/var/nix"
    mount --bind "$storage/nix" /nix
    [[ -f /etc/resolv.conf && ! -L /etc/resolv.conf ]]
    mount --bind ${filesystem}/etc/resolv.conf /etc/resolv.conf
    mount -o remount,bind,ro /etc/resolv.conf
    install -d -m 0755 "$root/export-root/nix/store" "$root/export-root/bin" "$root/export-root/etc/wrix" "$root/export-root/run/wrix-nix-pool" "$root/export-root/var/empty"
    install -d -m 0755 "$root/export-root/dev"
    touch "$root/export-root/dev/null"
    mount --bind /dev/null "$root/export-root/dev/null"
    mount --bind /nix/store "$root/export-root/nix/store"
    mount -o remount,bind,ro,nodev,nosuid "$root/export-root/nix/store"
    ln -s ${pkgs.bash}/bin/sh "$root/export-root/bin/sh"
    install -m 0444 ${filesystem}/etc/passwd "$root/export-root/etc/passwd"
    install -m 0444 ${filesystem}/etc/group "$root/export-root/etc/group"
    install -m 0444 ${filesystem}/etc/nsswitch.conf "$root/export-root/etc/nsswitch.conf"
    install -m 0444 ${filesystem}/etc/wrix/nix-pool-native.json "$root/export-root/etc/wrix/nix-pool-native.json"
    exec ${resources}/bin/wrix-nix-pool-daemon
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
    cp --remove-destination ${filesystem}/etc/resolv.conf etc/resolv.conf
    mkdir -p run var/empty var/lib/wrix/nix-pool
    chmod 0700 var/lib/wrix/nix-pool
    chmod 0755 nix/var/nix nix/var/nix/db
  '';
}
