{ pkgs, wrix }:

let
  image = wrix.nixPoolImage;
  krun = import ../../lib/sandbox/linux/krun-runtime.nix { linuxPkgs = pkgs; };
in
assert pkgs.stdenv.hostPlatform.isLinux;
assert image.source_kind == "nix-descriptor";
pkgs.writeShellApplication {
  name = "test-shared-nix-pool-export-linux";
  runtimeInputs = [
    pkgs.coreutils
    pkgs.jq
    pkgs.nix
    pkgs.openssh
    pkgs.podman
    pkgs.skopeo
    pkgs.sshfs
    pkgs.util-linux
  ];
  text = ''
    export WRIX_TEST_POOL_SOURCE=${image.source}
    export WRIX_TEST_POOL_RUNTIME=${krun}/bin/krun
    export WRIX_TEST_POOL_BASH=${pkgs.bashInteractive}
    exec ${pkgs.bash}/bin/bash ${./pool-export-linux.sh}
  '';
}
