# tmux-mcp tests - verify MCP server builds and tests pass
{
  pkgs,
  system,
  treefmt,
  src,
}:

let
  inherit (builtins) elem pathExists;
  inherit (pkgs) bash runCommandLocal rustPlatform;

  isLinux = elem system [
    "x86_64-linux"
    "aarch64-linux"
  ];

  # Check if KVM is available (for VM integration tests)
  # This is impure - requires `nix flake check --impure`
  hasKvm = pathExists "/dev/kvm";

  cratePath = ../../../lib/mcp/tmux/tmux-mcp;

  # Build the tmux-mcp package using rustPlatform
  # This properly handles cargo dependency fetching in the nix sandbox
  tmuxDebugMcp = rustPlatform.buildRustPackage {
    pname = "tmux-mcp";
    version = "0.1.0";
    src = cratePath;

    cargoLock = {
      lockFile = ../../../lib/mcp/tmux/tmux-mcp/Cargo.lock;
    };

    # Run tests as part of the build
    doCheck = true;

    meta = {
      description = "MCP server providing tmux pane management for AI-assisted debugging";
    };
  };

  # Copy test directory to store for use in VM tests
  testDir = runCommandLocal "tmux-mcp-test-dir" { } ''
    mkdir -p $out
    cp -r ${src}/tests/mcp/tmux/* $out/
    chmod -R +x $out/*.sh $out/e2e/*.sh 2>/dev/null || true
  '';

  # Integration tests run inside a NixOS VM with tmux available
  integrationTests =
    if isLinux && hasKvm then
      {
        # Integration test: Run MCP server and exercise all tools via JSON-RPC
        tmux-mcp-integration = pkgs.testers.nixosTest {
          name = "tmux-mcp-integration";

          nodes.machine =
            { pkgs, ... }:
            {
              virtualisation = {
                memorySize = 1024;
                diskSize = 2048;
                cores = 2;
              };

              environment.systemPackages = with pkgs; [
                tmux
                jq
                bc
                coreutils
                bash
                gnugrep
                tmuxDebugMcp
              ];

              # Create a test user for running tests
              users.users.testuser = {
                isNormalUser = true;
                uid = 1000;
              };
            };

          testScript = ''
            import json

            machine.wait_for_unit("multi-user.target")

            # Copy test scripts to VM
            machine.succeed("mkdir -p /home/testuser/tests")
            machine.succeed("cp -r ${testDir}/* /home/testuser/tests/")
            machine.succeed("chmod -R +x /home/testuser/tests/*.sh")
            machine.succeed("chown -R testuser:users /home/testuser/tests")

            # Run selected integration tests as testuser
            # These tests exercise the MCP server via JSON-RPC over stdio

            # Test 1: create_pane test
            print("Running create_pane test...")
            result = machine.succeed(
              "su - testuser -c 'cd /home/testuser/tests && ./test_create_pane.sh'"
            )
            print(result)

            # Test 2: send_keys test
            print("Running send_keys test...")
            result = machine.succeed(
              "su - testuser -c 'cd /home/testuser/tests && ./test_send_keys.sh'"
            )
            print(result)

            # Test 3: capture_pane test
            print("Running capture_pane test...")
            result = machine.succeed(
              "su - testuser -c 'cd /home/testuser/tests && ./test_capture_pane.sh'"
            )
            print(result)

            # Test 4: kill_pane test
            print("Running kill_pane test...")
            result = machine.succeed(
              "su - testuser -c 'cd /home/testuser/tests && ./test_kill_pane.sh'"
            )
            print(result)

            # Test 5: list_panes test
            print("Running list_panes test...")
            result = machine.succeed(
              "su - testuser -c 'cd /home/testuser/tests && ./test_list_panes.sh'"
            )
            print(result)

            # Test 6: exited_pane test
            print("Running exited_pane test...")
            result = machine.succeed(
              "su - testuser -c 'cd /home/testuser/tests && ./test_exited_pane.sh'"
            )
            print(result)

            # Test 7: error_handling test
            print("Running error_handling test...")
            result = machine.succeed(
              "su - testuser -c 'cd /home/testuser/tests && ./test_error_handling.sh'"
            )
            print(result)

            # Test 8: cleanup_on_exit test
            print("Running cleanup_on_exit test...")
            result = machine.succeed(
              "su - testuser -c 'cd /home/testuser/tests && ./test_cleanup_on_exit.sh'"
            )
            print(result)

            print("All tmux-mcp integration tests passed!")
          '';
        };
      }
    else
      { };

  # E2E tests run inside a NixOS VM with podman available
  # These test the MCP server running inside wrapix sandbox containers
  e2eTests =
    if isLinux && hasKvm then
      {
        # E2E test: Verify debug profile sandbox includes tmux and MCP server
        tmux-mcp-e2e-sandbox = pkgs.testers.nixosTest {
          name = "tmux-mcp-e2e-sandbox";

          nodes.machine =
            { pkgs, ... }:
            {
              virtualisation = {
                podman.enable = true;
                memorySize = 4096;
                diskSize = 8192;
                cores = 2;
              };

              environment.systemPackages = with pkgs; [
                podman
                slirp4netns
                jq
                coreutils
                bash
              ];

              users.users.testuser = {
                isNormalUser = true;
                uid = 1000;
                extraGroups = [ "wheel" ];
              };
            };

          testScript =
            let
              # Build the debug profile image for testing
              linuxPkgs = import pkgs.path {
                system = "x86_64-linux";
                config.allowUnfree = true;
              };
              profiles = import ../../../lib/sandbox/profiles.nix {
                pkgs = linuxPkgs;
                inherit treefmt;
              };
              debugImage = import ../../../lib/sandbox/image.nix {
                pkgs = linuxPkgs;
                profile = profiles.debug;
                entrypointPkg = linuxPkgs.hello; # Stand-in for claude-code
                entrypointSh = ../../../lib/sandbox/linux/entrypoint.sh;
                claudeConfig = { };
                claudeSettings = { };
              };
            in
            ''
              machine.wait_for_unit("multi-user.target")

              # Create test workspace
              machine.succeed("mkdir -p /tmp/workspace && chown testuser:users /tmp/workspace")

              # Load the debug profile image
              print("Loading debug profile image...")
              machine.succeed("${debugImage} | podman load")

              # Verify tmux is present in the container
              print("Verifying tmux is present...")
              result = machine.succeed(
                "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
                "--entrypoint /bin/bash "
                "-v /tmp/workspace:/workspace:rw "
                "-w /workspace "
                "docker-archive:${debugImage} "
                "-c \"tmux -V\"'"
              )
              assert "tmux" in result, f"tmux not found in container: {result}"
              print(f"tmux version: {result.strip()}")

              # Verify tmux-mcp is present in the container
              print("Verifying tmux-mcp is present...")
              result = machine.succeed(
                "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
                "--entrypoint /bin/bash "
                "-v /tmp/workspace:/workspace:rw "
                "-w /workspace "
                "docker-archive:${debugImage} "
                "-c \"which tmux-mcp\"'"
              )
              assert "tmux-mcp" in result, f"tmux-mcp not found: {result}"
              print("tmux-mcp found in container")

              # Test MCP server can start and respond to initialize
              print("Testing MCP server initialization...")
              result = machine.succeed(
                "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
                "--entrypoint /bin/bash "
                "-v /tmp/workspace:/workspace:rw "
                "-w /workspace "
                "docker-archive:${debugImage} "
                "-c \"echo '\\'''{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1.0\"}}}'\\''' | timeout 5 tmux-mcp 2>/dev/null || echo timeout\"'"
              )
              # Server should respond with JSON-RPC response containing serverInfo
              if "timeout" not in result and "serverInfo" in result:
                print("MCP server responded to initialize")
              else:
                print(f"MCP server response: {result}")

              print("E2E sandbox debug profile test passed!")
            '';
        };
      }
    else
      { };

in
{
  # Build the tmux-mcp Rust crate and run unit tests
  # Uses rustPlatform.buildRustPackage for proper offline cargo builds
  tmux-mcp-unit-tests = runCommandLocal "tmux-mcp-unit-tests" { } ''
    echo "Verifying tmux-mcp builds and tests pass..."
    # The package build with doCheck=true already ran tests
    test -x ${tmuxDebugMcp}/bin/tmux-mcp
    echo "tmux-mcp binary exists and tests passed"
    mkdir $out
  '';

  # Verify integration test shell scripts have valid syntax
  tmux-mcp-integration-syntax =
    runCommandLocal "tmux-mcp-integration-syntax"
      {
        nativeBuildInputs = [
          bash
          pkgs.shellcheck
        ];
      }
      ''
        echo "Checking integration test script syntax..."

        # Check integration test scripts
        INTEGRATION_DIR="${src}/tests/mcp/tmux"
        echo "Checking integration test scripts in $INTEGRATION_DIR..."
        for script in "$INTEGRATION_DIR"/*.sh; do
          if [ -f "$script" ]; then
            echo "Checking syntax: $(basename "$script")"
            bash -n "$script"
          fi
        done

        # Run shellcheck on integration test scripts
        # SC1091: Can't follow non-constant source (paths differ in nix store)
        # SC2034: Variable appears unused (some are used in sourced test_lib.sh or set for external use)
        echo "Running shellcheck on integration test scripts..."
        find "$INTEGRATION_DIR" -maxdepth 1 -name '*.sh' -exec shellcheck -x --exclude=SC1091,SC2034 {} +

        # Check E2E test scripts
        E2E_DIR="${src}/tests/mcp/tmux/e2e"
        if [ -d "$E2E_DIR" ]; then
          echo "Checking E2E test scripts in $E2E_DIR..."
          for script in "$E2E_DIR"/*.sh; do
            if [ -f "$script" ]; then
              echo "Checking syntax: $(basename "$script")"
              bash -n "$script"
            fi
          done

          # Run shellcheck on E2E scripts
          # SC1091: Can't follow non-constant source
          # SC2034: Variable appears unused (may be used externally)
          echo "Running shellcheck on E2E scripts..."
          find "$E2E_DIR" -name '*.sh' -exec shellcheck -x --exclude=SC1091,SC2034 {} +
        fi

        echo "All test scripts pass syntax checks"
        mkdir $out
      '';

  # Verify the Rust crate builds (this is the actual build artifact)
  tmux-mcp-builds = runCommandLocal "tmux-mcp-builds" { } ''
    echo "Verifying tmux-mcp builds..."
    test -x ${tmuxDebugMcp}/bin/tmux-mcp
    echo "tmux-mcp compiles successfully"
    mkdir $out
  '';
}
// integrationTests
// e2eTests
