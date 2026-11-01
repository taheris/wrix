{ pkgs, wrix }:

let
  nativePackages = [
    pkgs.bash
    pkgs.binutils
    pkgs.beads
    pkgs.coreutils
    pkgs.dolt
    pkgs.gcc
    pkgs.git
    pkgs.jq
    pkgs.procps
    pkgs.iproute2
    pkgs.nix
    pkgs.openssl
    pkgs.python3
    pkgs.rustc
    pkgs.util-linux
    wrix.rustPackage.celler
  ];
in
pkgs.testers.runNixOSTest {
  name = "cache-celler-native-system";
  requiredFeatures.kvm = false;
  nodes.machine = {
    imports = [ (import ../lib/userspace-vm.nix { inherit pkgs; }) ];
    nix.settings.experimental-features = [
      "nix-command"
      "flakes"
    ];
    virtualisation = {
      memorySize = 2048;
      diskSize = 4096;
    };
    environment.systemPackages = nativePackages;
  };
  testScript = ''
    from datetime import timedelta
    from shlex import quote

    machine.start()
    machine.wait_for_unit("multi-user.target")
    listing = machine.succeed("${wrix.rustPackage.cacheBackendTests}/bin/celler-backend-tests --list --ignored")
    assert "publisher_reuses_rust_store_client: test" in listing
    assert "production_runtime_excludes_competing_owners_until_process_exit: test" in listing
    environment = (
        # Private backend mount namespaces replace /run, including the system
        # profile alias. Resolve every runtime tool through immutable paths.
        "PATH=${pkgs.lib.makeBinPath nativePackages} "
        "WRIX_TEST_EMULATED_NATIVE_SYSTEM=1 "
        "WRIX_TEST_CELLERD=${wrix.rustPackage.celler}/bin/cellerd "
        # Resolve entries exactly as prepare-native-fixtures.sh does. The bundle
        # alias is not the native compiler identity accepted by readiness.
        'WRIX_TEST_CONTROLLER_BINDING="$(${pkgs.coreutils}/bin/readlink "${wrix.rustPackage.testFixtures}/controller-binding")" '
        'WRIX_TEST_SCCACHE="$(${pkgs.coreutils}/bin/readlink "${wrix.rustPackage.testFixtures}/compiler")" '
        'WRIX_TEST_SQL_CLIENT="$(${pkgs.coreutils}/bin/readlink "${wrix.rustPackage.testFixtures}/sql-client")" '
        "WRIX_REAL_BD_BIN=${pkgs.beads}/bin/bd "
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
    for test in [
        "execution_restart_retains_unbound_launch_and_ignores_workspace_completion",
        "execution_unavailable_ambiguous_and_failed_removal_observations_retain_binding",
    ]:
        print(machine.succeed(
            environment + "${wrix.rustPackage.cacheBackendTests}/bin/execution-lifetime-tests "
            + "--include-ignored --exact " + test + " --nocapture",
            timeout=timedelta(minutes=2),
        ))
    print(machine.succeed(
        environment + "${wrix.rustPackage.cacheBackendTests}/bin/workspace-admission-tests "
        "nix_store::admission:: --test-threads=1 --nocapture",
        timeout=timedelta(minutes=3),
    ))
    for binary in ["host-publication-only-tests", "host-signed-exclusion-tests"]:
        print(machine.succeed(
            environment + "${wrix.rustPackage.cacheBackendTests}/bin/" + binary
            + " --test-threads=1 --nocapture",
            timeout=timedelta(minutes=5),
        ))
    service_tests = "${wrix.rustPackage.cacheBackendTests}/bin/service-lifecycle-tests"
    cases = [
        line.removesuffix(": test")
        for line in machine.succeed(service_tests + " --list").splitlines()
        if line.endswith(": test")
    ]
    assert cases and len(cases) == len(set(cases))
    native_shared = "public_cli_shared_native_backend_preserves_namespaces_and_denies_peer_sql"
    early = [
        "workspace_stop_retires_current_native_read_issuance_without_stopping_controller",
        native_shared,
    ]
    assert all(test in cases for test in early)
    assert "launcher_fails_before_agent_start_without_a_guest_cache_address" in cases
    cases = early + [test for test in cases if test not in early]
    # Match Nextest's process isolation and retain every applicable case,
    # including ignored native cases. Emit each result before the next starts.
    print(f"Running all {len(cases)} native Linux service cases", flush=True)
    for test in cases:
        print(machine.succeed(
            environment + service_tests + " --include-ignored --exact " + quote(test) + " --nocapture",
            timeout=timedelta(minutes=17 if test == native_shared else 5),
        ), flush=True)
    machine.succeed("mkdir -p /etc/wrix; printf 'direct\\n' > /etc/wrix/image-agent")
    print(machine.succeed(
        environment + "${wrix.rustPackage.cacheBackendTests}/bin/workspace-admission-tests "
        "--include-ignored --exact nix_store::admission::"
        "worker_pool_issuance_denies_library_access_despite_forged_home_and_existing_approval --nocapture",
        timeout=timedelta(minutes=3),
    ))
  '';
}
