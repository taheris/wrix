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

  # Ralph utility function tests run on all platforms
  ralphTests = import ./ralph { inherit pkgs; };

  # Ralph template validation check (runs as part of nix flake check)
  # Uses mkTemplatesCheck from lib/ralph to validate all templates
  ralphTemplatesCheck =
    let
      ralph = import ../lib/ralph {
        inherit pkgs;
        mkSandbox = null; # not needed for template validation
      };
    in
    {
      ralph-templates = ralph.mkTemplatesCheck;
    };

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

  # Gas City tests (layered: eval, provider, lifecycle)
  cityTests = import ./city/unit.nix { inherit pkgs system treefmt; };

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
    inherit (wrapix) loomPackage;
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

  # Gas City integration test (shell-based, requires podman at runtime)
  cityIntegration = import ./city/integration.nix {
    inherit
      pkgs
      system
      linuxPkgs
      treefmt
      ;
  };

  # Ralph standalone container integration test (requires podman at runtime)
  ralphContainerIntegration = import ./ralph/container.nix {
    inherit
      pkgs
      system
      linuxPkgs
      treefmt
      ;
  };

  # README example verification
  readmeTest = {
    readme = import ./readme.nix { inherit pkgs src; };
  };

  # All checks combined
  checks =
    cityTests
    // darwinMountTests
    // darwinNetworkTests
    // darwinUidTests
    // ralphTemplatesCheck
    // ralphTests
    // readmeTest
    // rustChecks
    // sandboxIntegrationTests
    // shellTests
    // smokeTests
    // tmuxMcpTests
    // tomlTests;

  # ============================================================================
  # Integration Test Runners (require runtime environment)
  # ============================================================================

  # Ralph workflow integration tests (with mock-claude)
  # Copy entire ralph test directory to store so run-tests.sh can find mock-claude and scenarios
  ralphTestDir = pkgs.runCommandLocal "ralph-test-dir" { } ''
    cp -r ${./ralph} $out
    chmod -R u+w $out
    # Bundle shared test lib so fixtures.sh can source dolt-server.sh from the store
    mkdir -p $out/lib-shared
    cp ${./lib/dolt-server.sh} $out/lib-shared/dolt-server.sh
    chmod +x $out/run-tests.sh $out/mock-claude $out/scenarios/*.sh
    # Make standalone test scripts executable if they exist
    for f in $out/test-*.sh; do
      [ -f "$f" ] && chmod +x "$f"
    done
  '';

  # Get ralph scripts for RALPH_METADATA_DIR (contains variables.json, templates.json)
  ralphModule = import ../lib/ralph {
    inherit pkgs;
    mkSandbox = null;
  };

  ralphIntegrationTests = writeShellScriptBin "test-ralph-integration" ''
    set -euo pipefail
    export REPO_ROOT="${src}"
    export RALPH_METADATA_DIR="${ralphModule.scripts}/share/ralph"
    export RALPH_TEMPLATE_DIR="${src}/lib/ralph/template"
    export PATH="${pkgs.beads}/bin:${pkgs.dolt}/bin:${pkgs.git}/bin:${ralphModule.scripts}/bin:$PATH"
    # dolt init requires an identity; provide one for test runs outside a devShell
    export DOLT_ROOT_PATH="$(mktemp -d -t wrapix-test-dolt-root-XXXXXX)"
    trap 'rm -rf "$DOLT_ROOT_PATH"' EXIT
    dolt config --global --add user.email "test@wrapix.local" >/dev/null
    dolt config --global --add user.name "wrapix-test" >/dev/null
    exec ${ralphTestDir}/run-tests.sh
  '';

  # ============================================================================
  # Test Runner Apps
  # ============================================================================

  # Ralph integration tests only
  testRalph = writeShellScriptBin "test-ralph" ''
    set -euo pipefail
    echo "=== Ralph Integration Tests ==="
    echo ""
    ${ralphIntegrationTests}/bin/test-ralph-integration
  '';

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
    # Gas City integration test (requires podman)
    city = {
      meta.description = "Run Gas City full ops loop integration test (requires podman)";
      type = "app";
      program = "${cityIntegration.script}/bin/test-city";
    };

    # Ralph integration tests
    ralph = {
      meta.description = "Run ralph integration tests only";
      type = "app";
      program = "${testRalph}/bin/test-ralph";
    };

    # Ralph standalone container integration test (requires podman)
    ralph-container = {
      meta.description = "Run ralph standalone container integration test (requires podman)";
      type = "app";
      program = "${ralphContainerIntegration.script}/bin/test-ralph-container";
    };

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
    cityIntegration
    cityTests
    darwinMountTests
    darwinNetworkTests
    darwinUidTests
    loomChecks
    ralphContainerIntegration
    ralphTemplatesCheck
    ralphTests
    readmeTest
    rustChecks
    sandboxIntegrationTests
    shellTests
    smokeTests
    tmuxMcpTests
    tomlTests
    ;
}
