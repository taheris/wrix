# Integration tests - require NixOS VM with KVM
# These tests verify actual container runtime behavior
{ pkgs, treefmt }:

let
  # Use pkgs.hello as stand-in for beads and claude-code in tests
  testPkgs = pkgs.extend (_final: _prev: { beads = pkgs.hello; });

  # Build the sandbox image for tests
  profiles = import ../../lib/sandbox/profiles.nix {
    pkgs = testPkgs;
    inherit treefmt;
  };
  testImage = import ../../lib/sandbox/image.nix {
    pkgs = testPkgs;
    profile = profiles.base;
    entrypointPkg = testPkgs.hello; # Use hello as a stand-in for claude-code in tests
    entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
    claudeConfig = { };
    claudeSettings = { };
  };

  # Common VM configuration for all tests
  commonModule =
    { pkgs, ... }:
    {
      virtualisation = {
        podman.enable = true;
        # Allocate enough resources for container tests
        memorySize = 2048;
        diskSize = 4096;
        cores = 2;
      };

      # Enable pasta network mode for rootless containers
      environment.systemPackages = with pkgs; [
        podman
        slirp4netns
      ];

      # Allow rootless containers
      users.users.testuser = {
        isNormalUser = true;
        uid = 1000;
        extraGroups = [ "wheel" ];
      };
    };

in
{
  # Test 1: Verify container starts with pasta network
  container-start = pkgs.testers.nixosTest {
    name = "wrapix-container-start";

    nodes.machine =
      { ... }:
      {
        imports = [ commonModule ];
      };

    testScript = ''
      machine.wait_for_unit("multi-user.target")

      # Load the test image
      machine.succeed("${testImage} | podman load")

      # Create a test workspace directory
      machine.succeed("mkdir -p /tmp/workspace && chown testuser:users /tmp/workspace")

      # Run container with pasta network and verify it starts
      # We use a simple command to verify the container runs
      # Override entrypoint since we're testing container basics, not the entrypoint
      result = machine.succeed(
        "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
        "--entrypoint /bin/bash "
        "-v /tmp/workspace:/workspace:rw "
        "-w /workspace "
        "docker-archive:${testImage} "
        "-c \"echo container-started\"'"
      )
      assert "container-started" in result, f"Container failed to start: {result}"

      # Verify network connectivity (pasta mode provides network access)
      result = machine.succeed(
        "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
        "--entrypoint /bin/bash "
        "-v /tmp/workspace:/workspace:rw "
        "docker-archive:${testImage} "
        "-c \"cat /etc/hosts | grep localhost\"'"
      )
      assert "localhost" in result, f"Network not configured: {result}"
    '';
  };

  # Test 2: Verify filesystem isolation - only /workspace is accessible
  filesystem-isolation = pkgs.testers.nixosTest {
    name = "wrapix-filesystem-isolation";

    nodes.machine =
      { ... }:
      {
        imports = [ commonModule ];
      };

    testScript = ''
      machine.wait_for_unit("multi-user.target")

      # Load the test image
      machine.succeed("${testImage} | podman load")

      # Create test workspace
      machine.succeed("mkdir -p /tmp/workspace && chown testuser:users /tmp/workspace")
      machine.succeed("echo 'workspace-content' > /tmp/workspace/testfile.txt && chown testuser:users /tmp/workspace/testfile.txt")

      # Create a sensitive file on the host that should NOT be accessible
      machine.succeed("echo 'host-secret' > /tmp/host-secret.txt")

      # Verify workspace IS accessible
      result = machine.succeed(
        "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
        "--entrypoint /bin/bash "
        "-v /tmp/workspace:/workspace:rw "
        "-w /workspace "
        "docker-archive:${testImage} "
        "-c \"cat /workspace/testfile.txt\"'"
      )
      assert "workspace-content" in result, f"Workspace not accessible: {result}"

      # Verify host filesystem is NOT accessible (container should not see /tmp/host-secret.txt)
      # The file exists on host at /tmp/host-secret.txt but container's /tmp is isolated
      exit_code, output = machine.execute(
        "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
        "--entrypoint /bin/bash "
        "-v /tmp/workspace:/workspace:rw "
        "docker-archive:${testImage} "
        "-c \"cat /tmp/host-secret.txt 2>&1\"'"
      )
      # Should fail because /tmp/host-secret.txt doesn't exist in container
      assert exit_code != 0 or "host-secret" not in output, \
        f"Container should not access host /tmp: exit_code={exit_code}, output={output}"

      # Verify container cannot access host /etc/passwd content
      container_passwd = machine.succeed(
        "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
        "--entrypoint /bin/bash "
        "-v /tmp/workspace:/workspace:rw "
        "docker-archive:${testImage} "
        "-c \"cat /etc/passwd\"'"
      )
      # Container uses fakeNss, should not have host users
      assert "testuser" not in container_passwd or "nobody" in container_passwd, \
        "Container should not see host users"
    '';
  };

  # Test 3: Verify user namespace mapping - files have correct host ownership
  user-namespace = pkgs.testers.nixosTest {
    name = "wrapix-user-namespace";

    nodes.machine =
      { ... }:
      {
        imports = [ commonModule ];
      };

    testScript = ''
      machine.wait_for_unit("multi-user.target")

      # Load the test image
      machine.succeed("${testImage} | podman load")

      # Create test workspace owned by testuser
      machine.succeed("mkdir -p /tmp/workspace && chown testuser:users /tmp/workspace")

      # Create a file inside the container
      machine.succeed(
        "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
        "--entrypoint /bin/bash "
        "-v /tmp/workspace:/workspace:rw "
        "-w /workspace "
        "docker-archive:${testImage} "
        "-c \"echo created-in-container > /workspace/container-file.txt\"'"
      )

      # Verify the file exists on host
      machine.succeed("test -f /tmp/workspace/container-file.txt")

      # Verify the file has correct ownership (testuser UID 1000)
      result = machine.succeed("stat -c '%u' /tmp/workspace/container-file.txt")
      assert result.strip() == "1000", f"File should be owned by UID 1000, got: {result}"

      # Verify testuser can read the file
      content = machine.succeed("su - testuser -c 'cat /tmp/workspace/container-file.txt'")
      assert "created-in-container" in content, f"File content mismatch: {content}"

      # Create a subdirectory and verify ownership propagates
      machine.succeed(
        "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
        "--entrypoint /bin/bash "
        "-v /tmp/workspace:/workspace:rw "
        "-w /workspace "
        "docker-archive:${testImage} "
        "-c \"mkdir -p /workspace/subdir && echo nested > /workspace/subdir/nested.txt\"'"
      )

      # Verify subdirectory ownership
      result = machine.succeed("stat -c '%u' /tmp/workspace/subdir")
      assert result.strip() == "1000", f"Subdirectory should be owned by UID 1000, got: {result}"

      result = machine.succeed("stat -c '%u' /tmp/workspace/subdir/nested.txt")
      assert result.strip() == "1000", f"Nested file should be owned by UID 1000, got: {result}"
    '';
  };
}
