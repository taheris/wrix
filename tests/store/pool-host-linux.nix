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
    export WRIX_TEST_POOL_NIX=${pkgs.nix}/bin/nix
    export WRIX_TEST_POOL_BASH=${pkgs.bashInteractive}
    export WRIX_TEST_POOL_CONTROLLER=${wrix.rustPackage.controllerPackage}/bin/wrix-host-controller
    ${wrix.rustPackage.cacheBackendTests}/bin/workspace-admission-tests \
      --ignored --exact \
      nix_store::admission::native_host_owner_survives_controller_loss_collects_and_resumes_recorded_software \
      --nocapture
    exec ${wrix.rustPackage.cacheBackendTests}/bin/workspace-admission-tests \
      --ignored --exact \
      nix_store::admission::request::native::supervised_controller_prepares_after_requester_exit_and_preserves_native_owner \
      --nocapture
  '';
}
