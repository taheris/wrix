{ pkgs, wrix }:

assert pkgs.stdenv.hostPlatform.isLinux;
pkgs.writeShellApplication {
  name = "test-shared-nix-host-owner-linux";
  runtimeInputs = [
    pkgs.coreutils
    pkgs.git
    pkgs.nix
    pkgs.openssh
    pkgs.podman
    pkgs.util-linux
  ];
  text = ''
    [[ -r /dev/kvm && -w /dev/kvm ]]
    test -f ${wrix.nixPoolImage.source}
    test -f ${wrix.nixPoolImage.digest}
    export WRIX_NIX_POOL_PACKAGE_BINDING=${wrix.rustPackage.poolBinding}
    export WRIX_NIX_POOL_HOST_TOOLS=${wrix.rustPackage.poolHostTools}
    exec ${wrix.rustPackage.cacheBackendTests}/bin/workspace-admission-tests \
      --ignored --exact \
      nix_store::admission::native_host_owner_survives_controller_loss_collects_and_resumes_recorded_software \
      --nocapture
  '';
}
