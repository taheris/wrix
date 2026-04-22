# Test entry point - exports checks and test runner app
{
  pkgs,
  system,
  linuxPkgs,
  treefmt,
  src,
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
      ;
  };

  # TOML utility tests
  tomlTests = import ./toml.nix { inherit pkgs; };

  # Gas City tests (layered: eval, provider, lifecycle)
  cityTests = import ./city/unit.nix { inherit pkgs system treefmt; };

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
  };

  # Individual test sets (for debugging/selective running)
  inherit
    cityIntegration
    cityTests
    darwinMountTests
    darwinNetworkTests
    darwinUidTests
    ralphContainerIntegration
    ralphTemplatesCheck
    ralphTests
    readmeTest
    sandboxIntegrationTests
    shellTests
    smokeTests
    tmuxMcpTests
    tomlTests
    ;
}
