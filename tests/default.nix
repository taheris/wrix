# Test entry point - exports checks and test runner app
{
  pkgs,
  system,
  linuxPkgs,
  treefmt,
  src,
  wrix,
  crane,
  fenix,
}:

let
  inherit (pkgs.lib) concatStringsSep;
  inherit (pkgs)
    bash
    coreutils
    gawk
    git
    gnugrep
    gnused
    jq
    netcat
    nix
    socat
    writeShellScriptBin
    ;

  # ============================================================================
  # Pure Nix Checks (run via `nix flake check`)
  # ============================================================================

  # Smoke tests run on all platforms
  smokeTests = import ./sandbox/smoke.nix {
    inherit
      pkgs
      system
      treefmt
      crane
      fenix
      ;
    serviceCli = wrix.rustPackage.wrix;
  };

  # Test sandbox image with `hello` as a stand-in for claude/beads.
  # Exposed as `packages.test-image-base` so the host-side podman
  # verifiers can `nix build .#test-image-base` without rebuilding the
  # full claude/beads closure.
  #
  # `basePerturbed` is the same image with a one-attr claudeConfig
  # difference — the streamLayeredImage customisation layer's hash
  # changes while every base-layer blob remains identical. The
  # image-install-delta-bounded verifier installs both and asserts
  # the platform store only takes on bytes for the changed top layer.
  testImages = {
    base = import ./sandbox/test-image.nix {
      pkgs = linuxPkgs;
      inherit treefmt;
    };
    basePerturbed = import ./sandbox/test-image.nix {
      pkgs = linuxPkgs;
      inherit treefmt;
      claudeConfig = {
        _wrix_delta_bounded_probe = "v2";
      };
    };
    baseDirect = import ./sandbox/test-image.nix {
      pkgs = linuxPkgs;
      inherit treefmt;
      agent = "direct";
    };
    basePi = import ./sandbox/test-image.nix {
      pkgs = linuxPkgs;
      inherit treefmt;
      agent = "pi";
    };
    # Same image but with `pkgs.nix` added to the profile's packages — a
    # nix-shipping profile. Consumed by tests/sandbox/nix-in-container.sh,
    # which drives live `nix develop`/`nix build` as the unprivileged
    # runtime user and asserts no store-permission failure (FR #13).
    nix = import ./sandbox/test-image.nix {
      pkgs = linuxPkgs;
      inherit treefmt;
      shipNix = true;
    };
  };

  # Shell utility tests run on all platforms
  shellTests = import ./sandbox/shell.nix { inherit pkgs; };

  # Darwin mount tests run on all platforms (test logic, not VM)
  darwinMountTests = import ./darwin/mounts.nix { inherit pkgs treefmt; };

  # Darwin network tests run on all platforms (test logic, not VM)
  darwinNetworkTests = import ./darwin/network.nix { inherit pkgs treefmt; };

  # Darwin UID mapping tests (verify unshare-based VirtioFS ownership fix)
  darwinUidTests = import ./darwin/uid.nix { inherit pkgs treefmt; };

  # tmux-mcp tests (Rust unit tests and shell script syntax)
  tmuxMcpTests = import ./mcp/tmux/check.nix {
    inherit
      pkgs
      system
      src
      wrix
      ;
  };

  # TOML utility tests
  tomlTests = import ./toml.nix { inherit pkgs; };

  # Profile-image runtime checks share a craneLib + linux-package set with
  # the standalone tests below. They verify the per-profile sandbox images
  # contain the expected agent runtime binary.
  sandboxImageChecks = import ./sandbox/image-checks.nix {
    inherit
      pkgs
      system
      linuxPkgs
      crane
      fenix
      treefmt
      ;
    serviceCli = wrix.rustPackage.wrix;
  };

  rustChecks = {
    tmux-mcp-clippy = wrix.tmuxMcpPackage.clippy;
    tmux-mcp-nextest = wrix.tmuxMcpPackage.nextest;
    wrix-rust-clippy = wrix.rustPackage.clippy;
    wrix-rust-nextest = wrix.rustPackage.nextest;
  };

  prePushSmokeTests = pkgs.lib.removeAttrs smokeTests [
    "image-builds"
    "linux-microvm-krun-detection"
    "linux-pasta-port-forwarding-disabled"
    "network-mode-configuration"
    "package-runtime-path"
    "package-script-syntax"
    "script-syntax"
  ];

  ciChecks = rustChecks // {
    inherit (smokeTests)
      image-builds
      linux-microvm-krun-detection
      linux-pasta-port-forwarding-disabled
      network-mode-configuration
      package-runtime-path
      package-script-syntax
      script-syntax
      ;
  };

  # README example verification
  readmeTest = {
    readme = import ./readme.nix { inherit pkgs src; };
  };

  # All checks combined
  checks =
    darwinMountTests
    // darwinNetworkTests
    // darwinUidTests
    // readmeTest
    // shellTests
    // prePushSmokeTests
    // tmuxMcpTests
    // tomlTests;

  # ============================================================================
  # Test Runner Apps
  # ============================================================================

  # Fast tests: nix flake check (lint, smoke, unit tests)
  testAll = writeShellScriptBin "test-all" ''
    set -euo pipefail
    exec ${pkgs.nix}/bin/nix flake check "$@"
  '';

  mkCiApp = package: executable: {
    name = executable;
    inherit package executable;
  };

  ciApps = [
    (mkCiApp sandboxImageChecks.wrixSpawnLoadTest "test-wrix-spawn-load")
    (mkCiApp sandboxImageChecks.imageInstallArchivelessTest "test-image-install-archiveless")
    (mkCiApp sandboxImageChecks.imageInstallRealSkopeoTest "test-image-install-real-skopeo")
    (mkCiApp sandboxImageChecks.imageInstallDigestSkipTest "test-image-install-digest-skip")
    (mkCiApp sandboxImageChecks.digestMatchesStoredIdTest "test-image-digest-matches-stored-id")
    (mkCiApp sandboxImageChecks.linuxImageArchivelessSourceTest "test-linux-image-archiveless-source")
    (mkCiApp sandboxImageChecks.imageDigestNoTarTest "test-image-digest-no-tar")
    (mkCiApp sandboxImageChecks.imageTierGraphTest "test-image-tier-graph")
    (mkCiApp sandboxImageChecks.imageNixConfigTest "test-image-nix-config")
    (mkCiApp sandboxImageChecks.imageCaCertificatesTest "test-image-ca-certificates")
    (mkCiApp sandboxImageChecks.imageEntrypointCommandTest "test-image-entrypoint-command")
    (mkCiApp sandboxImageChecks.imageTierMembershipTest "test-image-tier-membership")
    (mkCiApp sandboxImageChecks.wrixImagesSourceKindTest "test-wrix-images-source-kind")
    (mkCiApp sandboxImageChecks.wrixImageLabelsTest "test-wrix-image-labels")
    (mkCiApp sandboxImageChecks.claudeRuntimeNoopTest "test-claude-runtime-noop")
    (mkCiApp sandboxImageChecks.prekHooksClosureTest "test-prek-hooks-closure")
    (mkCiApp sandboxImageChecks.baseImageUniversalTest "test-base-image-universal")
    (mkCiApp sandboxImageChecks.entrypointResolverBaseTest "test-entrypoint-resolver-base")
    (mkCiApp sandboxImageChecks.baseImageHashStableTest "test-base-image-hash-stable")
    (mkCiApp sandboxImageChecks.stableProfileHashStableTest "test-stable-profile-hash-stable")
    (mkCiApp sandboxImageChecks.stableProfileMembershipTest "test-stable-profile-membership")
    (mkCiApp sandboxImageChecks.pinnedToolchainStableTest "test-pinned-toolchain-stable-tier")
    (mkCiApp sandboxImageChecks.downstreamChangeLeafOnlyTest "test-downstream-change-leaf-only")
    (mkCiApp sandboxImageChecks.archivelessGeneratedChangeTest "test-archiveless-generated-change")
    (mkCiApp sandboxImageChecks.agentTierIsolatedTest "test-agent-tier-isolated")
    (mkCiApp sandboxImageChecks.agentExclusiveTest "test-agent-exclusive")
    (mkCiApp sandboxImageChecks.agentPkgThreadedTest "test-agent-pkg-threaded")
    (mkCiApp sandboxImageChecks.iterationCostBoundedTest "test-iteration-cost-bounded")
    (mkCiApp sandboxImageChecks.customisationLayerBoundedTest "test-customisation-layer-bounded")
    (mkCiApp sandboxImageChecks.imageNixDbConsistentTest "test-image-nix-db-consistent")
    (mkCiApp sandboxImageChecks.imageNixDbNoDanglingTest "test-image-nix-db-no-dangling")
    (mkCiApp testProfilesBuildPackage "test-profiles-build-package")
  ];

  ciAppNameLines = concatStringsSep "\n" (map (app: "      ${app.name}") ciApps);
  ciAppDerivations = builtins.listToAttrs (
    map (app: {
      inherit (app) name;
      value = app.package;
    }) ciApps
  );
  testAppDerivations = ciAppDerivations // {
    test-notify = testNotify;
  };

  testCi = writeShellScriptBin "test-ci" ''
        set -euo pipefail

        ci_checks=(
          tmux-mcp-clippy
          tmux-mcp-nextest
          wrix-rust-clippy
          wrix-rust-nextest
          image-builds
          linux-microvm-krun-detection
          linux-pasta-port-forwarding-disabled
          network-mode-configuration
          package-runtime-path
          package-script-syntax
          script-syntax
        )
        ci_apps=(
    ${ciAppNameLines}
        )

        if [[ "''${1:-}" = "--list" ]]; then
          printf 'check %s\n' "''${ci_checks[@]}"
          printf 'app %s\n' "''${ci_apps[@]}"
          exit 0
        fi

        repo_root=$(${pkgs.git}/bin/git rev-parse --show-toplevel 2>/dev/null || pwd) # best-effort: allow running outside a git checkout.
        cd "$repo_root"

        failed=0
        run_step() {
          local name="$1"
          shift
          echo "=== $name ==="
          if "$@"; then
            echo "PASS: $name"
          else
            echo "FAIL: $name" >&2
            failed=$((failed + 1))
          fi
        }

        run_ci_app() {
          local app="$1"
          local runner
          runner=$(${pkgs.nix}/bin/nix build --no-link --print-out-paths --no-warn-dirty ".#legacyPackages.${system}.ciApps.$app")
          "$runner/bin/$app"
        }

        run_step "nix flake check" ${pkgs.nix}/bin/nix flake check --no-warn-dirty

        for check in "''${ci_checks[@]}"; do
          run_step "$check" ${pkgs.nix}/bin/nix build --no-link --no-warn-dirty ".#legacyPackages.${system}.ciChecks.$check"
        done

        for app in "''${ci_apps[@]}"; do
          run_step "$app" run_ci_app "$app"
        done

        if [[ "$failed" -ne 0 ]]; then
          echo "$failed CI step(s) failed" >&2
          exit 1
        fi
  '';

  # profiles.rust.buildPackage hash invariant verifies (specs/profiles.md).
  # Driven via `nix eval` against the live flake, so it runs outside the build
  # sandbox like the other tests/profiles/*.sh scripts. Wrapper resolves
  # REPO_ROOT from the caller's git toplevel and threads jq + nix onto PATH.
  testProfilesBuildPackage = writeShellScriptBin "test-profiles-build-package" ''
    set -euo pipefail
    : "''${REPO_ROOT:=$(${git}/bin/git -C "''${PWD}" rev-parse --show-toplevel)}"
    export REPO_ROOT
    export PATH="${jq}/bin:${git}/bin:${nix}/bin:$PATH"
    exec ${bash}/bin/bash "$REPO_ROOT/tests/profiles/build-package.sh" "$@"
  '';

  notifyClient = import ../lib/notify/client.nix { inherit pkgs; };

  testNotify = writeShellScriptBin "test-notify" ''
    set -euo pipefail
    : "''${REPO_ROOT:=$(${git}/bin/git -C "''${PWD}" rev-parse --show-toplevel)}"
    export REPO_ROOT
    export PATH="${notifyClient}/bin:${bash}/bin:${coreutils}/bin:${gawk}/bin:${git}/bin:${gnugrep}/bin:${gnused}/bin:${jq}/bin:${netcat}/bin:${nix}/bin:${socat}/bin:$PATH"
    exec ${bash}/bin/bash "$REPO_ROOT/tests/standalone/notify-test.sh" "$@"
  '';

in
{
  # Checks for `nix flake check`
  inherit checks;

  # App for `nix run .#test` — fast checks (~10s)
  app = {
    meta.description = "Run fast tests: nix flake check (lint, smoke, unit)";
    type = "app";
    program = "${testAll}/bin/test-all";
  };

  # Individual test apps for selective running
  apps = {
    # Linux-only verifier for the wrix-spawn image install transport
    # (specs/sandbox.md § Image install path). Drives the shared
    # `imageLoadStep` snippet (the same one `wrix spawn` runs) through
    # shim podman + skopeo binaries; on Darwin prints a skip.
    wrix-spawn-load = {
      meta.description = "Verify wrix-spawn skopeo install idempotence (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.wrixSpawnLoadTest}/bin/test-wrix-spawn-load";
    };

    claude-runtime-noop = {
      meta.description = "Verify bundled Claude sandbox image closure contains claude-code";
      type = "app";
      program = "${sandboxImageChecks.claudeRuntimeNoopTest}/bin/test-claude-runtime-noop";
    };

    # Linux-only verifier for the launcher's digest-preflight install skip
    # (specs/sandbox.md § Image install path; specs/image-builder.md). Drives
    # the shared `imageLoadStep` snippet through shim podman + skopeo binaries
    # and asserts the second install short-circuits when the image's content
    # digest is already present. Darwin prints a skip notice.
    image-install-digest-skip = {
      meta.description = "Verify launcher digest-preflight short-circuits image install (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.imageInstallDigestSkipTest}/bin/test-image-install-digest-skip";
    };

    image-install-real-skopeo = {
      meta.description = "Verify launcher image install against real packaged skopeo (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.imageInstallRealSkopeoTest}/bin/test-image-install-real-skopeo";
    };

    image-install-archiveless = {
      meta.description = "Verify Linux descriptor image install avoids archive transports.";
      type = "app";
      program = "${sandboxImageChecks.imageInstallArchivelessTest}/bin/test-image-install-archiveless";
    };

    image-digest-matches-stored-id = {
      meta.description = "Skip legacy docker-archive digest verifier for descriptor images.";
      type = "app";
      program = "${sandboxImageChecks.digestMatchesStoredIdTest}/bin/test-image-digest-matches-stored-id";
    };

    linux-image-archiveless-source = {
      meta.description = "Verify Linux profile images publish nix-descriptor sources.";
      type = "app";
      program = "${sandboxImageChecks.linuxImageArchivelessSourceTest}/bin/test-linux-image-archiveless-source";
    };

    image-digest-no-tar = {
      meta.description = "Verify Linux descriptor image digests do not depend on raw image archives.";
      type = "app";
      program = "${sandboxImageChecks.imageDigestNoTarTest}/bin/test-image-digest-no-tar";
    };

    image-tier-graph = {
      meta.description = "Verify profile images expose the base/stable/agent/leaf tier graph and source kind.";
      type = "app";
      program = "${sandboxImageChecks.imageTierGraphTest}/bin/test-image-tier-graph";
    };

    image-nix-config = {
      meta.description = "Verify baked profile images enable flakes and disable the in-container Nix sandbox.";
      type = "app";
      program = "${sandboxImageChecks.imageNixConfigTest}/bin/test-image-nix-config";
    };

    image-ca-certificates = {
      meta.description = "Verify baked profile images contain CA certificates and SSL_CERT_FILE points at them.";
      type = "app";
      program = "${sandboxImageChecks.imageCaCertificatesTest}/bin/test-image-ca-certificates";
    };

    image-entrypoint-command = {
      meta.description = "Verify the selected platform entrypoint is the image startup command.";
      type = "app";
      program = "${sandboxImageChecks.imageEntrypointCommandTest}/bin/test-image-entrypoint-command";
    };

    image-tier-membership = {
      meta.description = "Verify non-base profile-image tiers skip lower-tier closures.";
      type = "app";
      program = "${sandboxImageChecks.imageTierMembershipTest}/bin/test-image-tier-membership";
    };

    wrix-images-source-kind = {
      meta.description = "Verify built-in wrix profile images expose platform source kinds.";
      type = "app";
      program = "${sandboxImageChecks.wrixImagesSourceKindTest}/bin/test-wrix-images-source-kind";
    };

    wrix-image-labels = {
      meta.description = "Verify wrix-managed image labels on profile and support images.";
      type = "app";
      program = "${sandboxImageChecks.wrixImageLabelsTest}/bin/test-wrix-image-labels";
    };

    prek-hooks-closure = {
      meta.description = "Verify default sandbox image closure contains the prek hooks bundle";
      type = "app";
      program = "${sandboxImageChecks.prekHooksClosureTest}/bin/test-prek-hooks-closure";
    };

    base-image-universal = {
      meta.description = "Verify wrix-base-image holds only universal bottom-of-closure paths (no profile-specific rustc)";
      type = "app";
      program = "${sandboxImageChecks.baseImageUniversalTest}/bin/test-base-image-universal";
    };

    entrypoint-resolver-base = {
      meta.description = "Verify sandbox images include getent for entrypoint allowlist resolution";
      type = "app";
      program = "${sandboxImageChecks.entrypointResolverBaseTest}/bin/test-entrypoint-resolver-base";
    };

    base-image-hash-stable = {
      meta.description = "Verify wrix-base-image drvPath is invariant under profile-level input changes";
      type = "app";
      program = "${sandboxImageChecks.baseImageHashStableTest}/bin/test-base-image-hash-stable";
    };

    stable-profile-hash-stable = {
      meta.description = "Verify wrix-stable-profile-<name> drvPath is invariant under tier-2 input changes";
      type = "app";
      program = "${sandboxImageChecks.stableProfileHashStableTest}/bin/test-stable-profile-hash-stable";
    };

    stable-profile-membership = {
      meta.description = "Verify wrix-stable-profile-<name> excludes downstream packages and the agent runtime (tier-2 leaf)";
      type = "app";
      program = "${sandboxImageChecks.stableProfileMembershipTest}/bin/test-stable-profile-membership";
    };

    pinned-toolchain-stable-tier = {
      meta.description = "Verify a downstream-pinned rust toolchain lands in tier 1 (wrix-stable-profile-<name>), not the leaf";
      type = "app";
      program = "${sandboxImageChecks.pinnedToolchainStableTest}/bin/test-pinned-toolchain-stable-tier";
    };

    downstream-change-leaf-only = {
      meta.description = "Verify a leaf change leaves every tier-0, tier-1, and tier-2 layer blob byte-identical (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.downstreamChangeLeafOnlyTest}/bin/test-downstream-change-leaf-only";
    };

    archiveless-generated-change = {
      meta.description = "Verify generated metadata changes only the descriptor and top customisation layer.";
      type = "app";
      program = "${sandboxImageChecks.archivelessGeneratedChangeTest}/bin/test-archiveless-generated-change";
    };

    agent-tier-isolated = {
      meta.description = "Verify the agent runtime rides its own tier; an agent-version bump leaves tier-0 and tier-1 blobs byte-identical (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.agentTierIsolatedTest}/bin/test-agent-tier-isolated";
    };

    agent-exclusive = {
      meta.description = "Verify exactly one agent rides each image: a direct image has no claude-code, a claude image no direct runner";
      type = "app";
      program = "${sandboxImageChecks.agentExclusiveTest}/bin/test-agent-exclusive";
    };

    agent-pkg-threaded = {
      meta.description = "Verify agentPkg is threaded into the selected agent image closure";
      type = "app";
      program = "${sandboxImageChecks.agentPkgThreadedTest}/bin/test-agent-pkg-threaded";
    };

    iteration-cost-bounded = {
      meta.description = "Verify a one-wrapper-script perturbation only re-emits the customisation layer + dependent top layers (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.iterationCostBoundedTest}/bin/test-iteration-cost-bounded";
    };

    customisation-layer-bounded = {
      meta.description = "Verify the customisation layer elides Nix's 8 MiB gc-reserved-space padding and stays bounded (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.customisationLayerBoundedTest}/bin/test-customisation-layer-bounded";
    };

    image-nix-db-consistent = {
      meta.description = "Verify the baked image's Nix DB registers its full on-disk store with no orphan (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.imageNixDbConsistentTest}/bin/test-image-nix-db-consistent";
    };

    image-nix-db-no-dangling = {
      meta.description = "Verify the baked image's Nix DB registers no dangling (registered-but-absent) path (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.imageNixDbNoDanglingTest}/bin/test-image-nix-db-no-dangling";
    };

    ci = {
      meta.description = "Run full CI-only image and profile verifiers.";
      type = "app";
      program = "${testCi}/bin/test-ci";
    };

    notify = {
      meta.description = "Verify notification client, daemon, and container transport contracts.";
      type = "app";
      program = "${testNotify}/bin/test-notify";
    };

    # profiles.rust.buildPackage [verify] hash invariants (specs/profiles.md).
    profiles-build-package = {
      meta.description = "Verify profiles.rust.buildPackage hash invariants (bin/clippy/nextest/cargoArtifacts)";
      type = "app";
      program = "${testProfilesBuildPackage}/bin/test-profiles-build-package";
    };
  };

  # Individual test sets (for debugging/selective running)
  inherit
    darwinMountTests
    darwinNetworkTests
    darwinUidTests
    readmeTest
    ciAppDerivations
    testAppDerivations
    ciChecks
    rustChecks
    shellTests
    smokeTests
    testCi
    testImages
    tmuxMcpTests
    tomlTests
    ;
}
