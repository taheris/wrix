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

  # Loom container smoke runner (Linux: real podman smoke; Darwin: skip stub).
  # Unit + integration coverage for loom comes from the loom-clippy /
  # loom-nextest entries below, which reuse the cargoArtifacts of
  # wrapix.loomPackage so flake check shares dep compilation with the
  # devshell's packages.loom build.
  loomDeriv = import ./loom {
    inherit
      pkgs
      system
      linuxPkgs
      fenix
      treefmt
      ;
    inherit (wrapix) loomPackage loomLinuxPackage;
  };

  # Per specs/profiles.md, the rust profile's buildPackage emits separate
  # clippy/nextest derivations from the same buildPackage call that produces
  # packages.loom / packages.tmux-mcp; cargoArtifacts is shared so editing a
  # workspace .rs file invalidates lint/test/bin together but reuses the dep
  # cache, and editing a file under extraSrcs (e.g. tests/loom/mock-pi)
  # invalidates only loom-clippy/loom-nextest.
  rustChecks = {
    loom-clippy = wrapix.loomPackage.clippy;
    # loom-tests invokes `loom gate verify` across every spec under
    # `specs/*.md` for the `[check]` and `[test]` tiers (see Nix
    # Integration in specs/loom-tests.md). It complements
    # `loom-nextest` (bare `cargo nextest run`) — the gate-driven
    # variant batches only the annotated `[test]` targets and runs
    # the static `[check]` walks alongside.
    loom-nextest = loomDeriv.nextestFast;
    loom-tests = loomDeriv.loomTests;
    tmux-mcp-clippy = wrapix.tmuxMcpPackage.clippy;
    tmux-mcp-nextest = wrapix.tmuxMcpPackage.nextest;
  };

  # `nix build .#loom-tests` exposes the gate-driven derivation
  # individually (matches the `packages.loom-tests` lift in
  # modules/flake/tests.nix).
  loomChecks = {
    loom-tests = loomDeriv.loomTests;
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
    # Loom property tests + container smoke. Property tests run everywhere;
    # the smoke runs on Linux only (Darwin prints a skip notice).
    loom = {
      meta.description = "Run loom property tests + container smoke (Linux: requires podman; Darwin: smoke skipped)";
      type = "app";
      program = "${loomDeriv.testLoom}/bin/test-loom";
    };

    # Linux-only verifier for the wrapix-spawn image-source -> podman-load
    # contract. Drives the shared `imageLoadStep` snippet (the same one
    # `wrapix spawn` runs) through a shim podman; on Darwin prints a skip.
    wrapix-spawn-load = {
      meta.description = "Verify wrapix-spawn image-source -> podman-load idempotence (Linux only)";
      type = "app";
      program = "${loomDeriv.wrapixSpawnLoadTest}/bin/test-wrapix-spawn-load";
    };

    pi-runtime-image = {
      meta.description = "Verify sandbox-pi image closure contains executable pi-mono binary";
      type = "app";
      program = "${loomDeriv.piRuntimeImageTest}/bin/test-pi-runtime-image";
    };

    claude-runtime-noop = {
      meta.description = "Verify default sandbox image closure has claude-code but not pi-mono";
      type = "app";
      program = "${loomDeriv.claudeRuntimeNoopTest}/bin/test-claude-runtime-noop";
    };

    direct-runtime-image = {
      meta.description = "Verify sandbox-direct image closure contains executable loom-direct-runner binary";
      type = "app";
      program = "${loomDeriv.directRuntimeImageTest}/bin/test-direct-runtime-image";
    };

    # profiles.rust.buildPackage [verify] hash invariants (specs/profiles.md
    # success criteria 414-427). Runs all 7 test functions in
    # tests/profiles/build-package.sh.
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
    loomChecks
    readmeTest
    rustChecks
    sandboxIntegrationTests
    shellTests
    smokeTests
    tmuxMcpTests
    tomlTests
    ;
}
