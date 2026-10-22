{ pkgs, wrix }:

pkgs.testers.runNixOSTest {
  name = "cache-celler-native-system";
  requiredFeatures.kvm = false;
  nodes.machine = {
    imports = [ (import ../lib/userspace-vm.nix { inherit pkgs; }) ];
    virtualisation = {
      memorySize = 2048;
      diskSize = 4096;
    };
    environment.systemPackages = [
      pkgs.bash
      pkgs.beads
      pkgs.coreutils
      pkgs.git
      pkgs.jq
      pkgs.procps
      pkgs.iproute2
      pkgs.nix
      pkgs.openssl
      pkgs.python3
      pkgs.util-linux
      wrix.rustPackage.celler
    ];
  };
  testScript = ''
    from datetime import timedelta

    machine.start()
    machine.wait_for_unit("multi-user.target")
    listing = machine.succeed("${wrix.rustPackage.cacheBackendTests}/bin/celler-backend-tests --list --ignored")
    assert "publisher_reuses_rust_store_client: test" in listing
    assert "production_runtime_excludes_competing_owners_until_process_exit: test" in listing
    environment = (
        "WRIX_TEST_CELLERD=${wrix.rustPackage.celler}/bin/cellerd "
        "WRIX_TEST_CONTROLLER_BINDING=${wrix.rustPackage.testFixtures}/controller-binding "
        "WRIX_TEST_ISOLATED_STORE_SCRIPT=${../cache/isolated-store.sh} "
        "WRIX_TEST_RUNTIME_LAYOUT_SCRIPT=${../cache/runtime-layout.sh} "
        "WRIX_TEST_HOST_BOOTSTRAP_SCRIPT=${../../lib/services/host-nix-config.sh} "
        "WRIX_TEST_HOST_CACHE_POLICY_BIN=${wrix.rustPackage.hostCachePolicy}/bin/wrix-host-cache-policy "
    )
    print(machine.succeed(
        environment + "${wrix.rustPackage.cacheBackendTests}/bin/celler-backend-tests "
        "--include-ignored --test-threads=1 --nocapture",
        timeout=timedelta(minutes=15),
    ))
    print(machine.succeed(
        environment + "${wrix.rustPackage.cacheBackendTests}/bin/celler-auth-tests "
        "--test-threads=1 --nocapture",
        timeout=timedelta(minutes=5),
    ))
    print(machine.succeed(
        environment + "${wrix.rustPackage.cacheBackendTests}/bin/service-lifecycle-tests "
        "--include-ignored --test-threads=1 --nocapture",
        timeout=timedelta(minutes=15),
    ))
    for test in [
        "execution_restart_retains_unbound_launch_and_ignores_workspace_completion",
        "execution_unavailable_ambiguous_and_failed_removal_observations_retain_binding",
    ]:
        print(machine.succeed(
            "${wrix.rustPackage.cacheBackendTests}/bin/execution-lifetime-tests "
            + "--include-ignored --exact " + test + " --nocapture",
            timeout=timedelta(minutes=2),
        ))
    for binary in ["host-publication-only-tests", "host-signed-exclusion-tests"]:
        print(machine.succeed(
            environment + "${wrix.rustPackage.cacheBackendTests}/bin/" + binary
            + " --test-threads=1 --nocapture",
            timeout=timedelta(minutes=5),
        ))
  '';
}
