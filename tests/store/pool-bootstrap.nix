{ pkgs, wrix }:

let
  image = wrix.nixPoolImage;
  dnsFixtureCheck = pkgs.runCommand "pool-dns-fixture-conformance" { } ''
    set -euo pipefail
    ${pkgs.python3}/bin/python ${./egress-server.py} --test
    touch "$out"
  '';
  archive =
    if image.source_kind == "docker-archive" then
      image.source
    else
      assert image.source_kind == "nix-descriptor";
      pkgs.runCommand "nix-pool-image-archive"
        {
          nativeBuildInputs = [
            pkgs.jq
            pkgs.skopeo
          ];
        }
        ''
          set -euo pipefail
          export HOME="$TMPDIR"
          layout=$(jq -r '.oci_layout' ${image.source})
          ref=$(jq -r '.oci_ref' ${image.source})
          skopeo --insecure-policy copy --quiet "oci:$layout:$ref" "docker-archive:$out"
        '';
  bundle =
    pkgs.runCommand "nix-pool-bootstrap-image"
      {
        nativeBuildInputs = [
          pkgs.gnutar
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        mkdir archive root "$out"
        tar -xf ${archive} -C archive
        config=$(jq -r '.[0].Config' archive/manifest.json)
        jq -e '
          (.config.Entrypoint | length) == 1 and
          (.config.Entrypoint[0] | startswith("/nix/store/") and endswith("/bin/wrix-nix-pool")) and
          .config.Labels["wrix.image.kind"] == "service" and
          .config.Labels["wrix.nix-pool.bootstrap-version"] == "2"
        ' "archive/$config"
        jq -r '.[0].Layers[]' archive/manifest.json > layers
        while IFS= read -r layer; do
          tar --same-permissions --no-same-owner -xf "archive/$layer" -C root
        done < layers
        if find root -name '.wh.*' | grep -q .; then
          printf 'Unexpected whiteout in the fresh pool image.\n' >&2
          exit 1
        fi
        tar --owner=0 --group=0 --numeric-owner -cf "$out/rootfs.tar" -C root .
        cp "archive/$config" "$out/config.json"
      '';
  exercise = pkgs.writeShellScript "exercise-nix-pool-bootstrap" (
    builtins.readFile ./pool-bootstrap.sh
  );
  finish = pkgs.writeShellScript "finish-nix-pool-root" ''
    set -euo pipefail
    cd /
    exec </dev/null
    umount --recursive /.oldroot
    rmdir /.oldroot
    exec /bin/bash /tmp/exercise-bootstrap "$(</tmp/pool-entrypoint)"
  '';
  enter = pkgs.writeShellScript "enter-nix-pool-service" ''
    set -euo pipefail
    root=/srv/nix-pool-service
    mount --make-rprivate /
    mount --bind "$root" "$root"
    mkdir -p "$root/proc" "$root/dev" "$root/sys"
    mount -t sysfs sysfs "$root/sys"
    mount -t proc proc "$root/proc"
    mount --rbind /dev "$root/dev"
    mount --make-rslave "$root/dev"
    cp ${exercise} "$root/tmp/exercise-bootstrap"
    cp ${finish} "$root/tmp/finish-pivot"
    mapfile -t image_env < <(${pkgs.jq}/bin/jq -r '.config.Env[]' ${bundle}/config.json)
    ${pkgs.jq}/bin/jq -r '.config.Entrypoint[0]' ${bundle}/config.json > "$root/tmp/pool-entrypoint"
    mkdir "$root/.oldroot"
    cd "$root"
    pivot_root . .oldroot
    exec /usr/bin/env -i "''${image_env[@]}" /bin/bash /tmp/finish-pivot
  '';
in
pkgs.testers.runNixOSTest {
  name = "shared-nix-pool-bootstrap";
  requiredFeatures.kvm = false;
  nodes.machine = {
    imports = [ (import ../lib/userspace-vm.nix { inherit pkgs; }) ];
    virtualisation = {
      memorySize = 2048;
      diskSize = 12288;
      cores = 2;
    };
    boot.kernelModules = [
      "loop"
      "ext4"
    ];
    systemd.services.pool-egress-control = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig.ExecStart = "${pkgs.python3}/bin/python ${./egress-server.py}";
    };
    environment.systemPackages = [
      pkgs.bash
      pkgs.coreutils
      pkgs.gnutar
      pkgs.util-linux
    ];
  };
  testScript = ''
    from datetime import timedelta

    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("pool-egress-control.service")
    machine.succeed("test -e ${dnsFixtureCheck}")
    machine.succeed("ip address add 93.184.216.34/32 dev lo")
    machine.succeed("ip address add 1.1.1.1/32 dev lo")
    machine.succeed("mkdir -p /srv/nix-pool-service; tar --same-permissions -xf ${bundle}/rootfs.tar -C /srv/nix-pool-service")
    print(machine.succeed(
        "unshare --pid --fork unshare --mount ${enter}",
        timeout=timedelta(minutes=5),
    ))
  '';
}
