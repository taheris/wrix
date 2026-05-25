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
  inherit (builtins) elem pathExists;
  inherit (pkgs) writeShellScriptBin;
  isLinux = elem system [
    "x86_64-linux"
    "aarch64-linux"
  ];

  # Check if KVM is available (for VM integration tests)
  # This is impure - requires `nix flake check --impure`
  hasKvm = pathExists "/dev/kvm";

  # ============================================================================
  # Pure Nix Checks (run via `nix flake check`)
  # ============================================================================

  # Smoke tests run on all platforms
  smokeTests = import ./sandbox/smoke.nix { inherit pkgs system treefmt; };

  # Sandbox VM integration tests require NixOS VM (Linux with KVM only)
  # Skip when KVM unavailable (e.g., inside containers)
  sandboxIntegrationTests =
    if isLinux && hasKvm then import ./sandbox/integration.nix { inherit pkgs treefmt; } else { };

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
      treefmt
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
    // sandboxIntegrationTests
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
    # Linux-only verifier for the wrapix-spawn image-source -> podman-load
    # contract. Drives the shared `imageLoadStep` snippet (the same one
    # `wrapix spawn` runs) through a shim podman; on Darwin prints a skip.
    wrapix-spawn-load = {
      meta.description = "Verify wrapix-spawn image-source -> podman-load idempotence (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.wrapixSpawnLoadTest}/bin/test-wrapix-spawn-load";
    };

    pi-runtime-image = {
      meta.description = "Verify sandbox-pi image closure contains executable pi-mono binary";
      type = "app";
      program = "${sandboxImageChecks.piRuntimeImageTest}/bin/test-pi-runtime-image";
    };

    claude-runtime-noop = {
      meta.description = "Verify default sandbox image closure has claude-code but not pi-mono";
      type = "app";
      program = "${sandboxImageChecks.claudeRuntimeNoopTest}/bin/test-claude-runtime-noop";
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
    sandboxIntegrationTests
    shellTests
    smokeTests
    tmuxMcpTests
    tomlTests
    ;
}
