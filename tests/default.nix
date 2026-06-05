# Test entry point - exports checks and test runner app
{
  pkgs,
  system,
  linuxPkgs,
  treefmt,
  src,
  wrapix,
  fenix ? null,
}:

let
  inherit (pkgs) writeShellScriptBin;

  # ============================================================================
  # Pure Nix Checks (run via `nix flake check`)
  # ============================================================================

  # Smoke tests run on all platforms
  smokeTests = import ./sandbox/smoke.nix { inherit pkgs system treefmt; };

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
        _wrapix_delta_bounded_probe = "v2";
      };
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
      wrapix
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
      fenix
      treefmt
      ;
  };

  rustChecks = {
    tmux-mcp-clippy = wrapix.tmuxMcpPackage.clippy;
    tmux-mcp-nextest = wrapix.tmuxMcpPackage.nextest;
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
    // rustChecks
    // shellTests
    // smokeTests
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

  # profiles.rust.buildPackage hash invariant verifies (specs/profiles.md).
  # Driven via `nix eval` against the live flake, so it runs outside the build
  # sandbox like the other tests/profiles/*.sh scripts. Wrapper resolves
  # REPO_ROOT from the caller's git toplevel and threads jq + nix onto PATH.
  testProfilesBuildPackage = writeShellScriptBin "test-profiles-build-package" ''
    set -euo pipefail
    : "''${REPO_ROOT:=$(${pkgs.git}/bin/git -C "''${PWD}" rev-parse --show-toplevel)}"
    export REPO_ROOT
    export PATH="${pkgs.jq}/bin:${pkgs.git}/bin:${pkgs.nix}/bin:$PATH"
    exec ${pkgs.bash}/bin/bash "$REPO_ROOT/tests/profiles/build-package.sh" "$@"
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
    # Linux-only verifier for the wrapix-spawn image install transport
    # (specs/sandbox.md § Image install path). Drives the shared
    # `imageLoadStep` snippet (the same one `wrapix spawn` runs) through
    # shim podman + skopeo binaries; on Darwin prints a skip.
    wrapix-spawn-load = {
      meta.description = "Verify wrapix-spawn skopeo install idempotence (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.wrapixSpawnLoadTest}/bin/test-wrapix-spawn-load";
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

    # Faithful counterpart to image-install-digest-skip: drives the real
    # `docker-archive → oci-archive` skopeo conversion on the live image and
    # asserts `image.digest` equals the post-conversion OCI config digest (the
    # value podman stores as `.Id`). Guards against a digest taken from the
    # docker-archive manifest, which never matches and re-streams every launch.
    image-digest-matches-stored-id = {
      meta.description = "Verify image.digest equals the post-conversion OCI config digest (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.digestMatchesStoredIdTest}/bin/test-image-digest-matches-stored-id";
    };

    prek-hooks-closure = {
      meta.description = "Verify default sandbox image closure contains the prek hooks bundle";
      type = "app";
      program = "${sandboxImageChecks.prekHooksClosureTest}/bin/test-prek-hooks-closure";
    };

    base-image-universal = {
      meta.description = "Verify wrapix-base-image holds only universal bottom-of-closure paths (no profile-specific rustc)";
      type = "app";
      program = "${sandboxImageChecks.baseImageUniversalTest}/bin/test-base-image-universal";
    };

    base-image-hash-stable = {
      meta.description = "Verify wrapix-base-image drvPath is invariant under profile-level input changes";
      type = "app";
      program = "${sandboxImageChecks.baseImageHashStableTest}/bin/test-base-image-hash-stable";
    };

    stable-profile-hash-stable = {
      meta.description = "Verify wrapix-stable-profile-<name> drvPath is invariant under tier-2 input changes";
      type = "app";
      program = "${sandboxImageChecks.stableProfileHashStableTest}/bin/test-stable-profile-hash-stable";
    };

    stable-profile-membership = {
      meta.description = "Verify wrapix-stable-profile-<name> excludes downstream packages and the agent runtime (tier-2 leaf)";
      type = "app";
      program = "${sandboxImageChecks.stableProfileMembershipTest}/bin/test-stable-profile-membership";
    };

    pinned-toolchain-stable-tier = {
      meta.description = "Verify a downstream-pinned rust toolchain lands in tier 1 (wrapix-stable-profile-<name>), not the leaf";
      type = "app";
      program = "${sandboxImageChecks.pinnedToolchainStableTest}/bin/test-pinned-toolchain-stable-tier";
    };

    downstream-change-leaf-only = {
      meta.description = "Verify a leaf change leaves every tier-0, tier-1, and tier-2 layer blob byte-identical (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.downstreamChangeLeafOnlyTest}/bin/test-downstream-change-leaf-only";
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
    rustChecks
    shellTests
    smokeTests
    testImages
    tmuxMcpTests
    tomlTests
    ;
}
