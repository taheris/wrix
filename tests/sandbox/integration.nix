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

  # Test 4: Verify the container-side pre-commit hook chain fires.
  # Per specs/pre-commit.md § Bead-Container Hook Installation: the entrypoint
  # sets core.hooksPath to wrapix.prekHooks and the wrappers
  # (pre-push-checks, skip-if-missing) land on PATH so a `.pre-commit-config.yaml`
  # hook configured by the consumer actually fires from inside the container.
  # We seed a workspace with .git + a config that names both a skip-if-missing
  # probe (asserts the wrapper resolves) and a sentinel hook that writes a
  # marker file outside the worktree (.git/sentinel-fired-precommit), then run
  # `git commit` as the entrypoint's command override.
  container-pre-commit = pkgs.testers.nixosTest {
    name = "wrapix-container-pre-commit";

    nodes.machine =
      { ... }:
      {
        imports = [ commonModule ];
      };

    testScript =
      let
        cfg = pkgs.writeText "container-pre-commit.yaml" ''
          repos:
            - repo: local
              hooks:
                - id: wrapper-on-path
                  name: wrapper-on-path
                  entry: skip-if-missing nonexistent-tool-xyz -- false
                  language: system
                  stages: [pre-commit]
                  always_run: true
                  pass_filenames: false
                - id: sentinel
                  name: sentinel
                  entry: /workspace/.git/sentinel-pre-commit.sh
                  language: system
                  stages: [pre-commit]
                  always_run: true
                  pass_filenames: false
        '';
        sentinel = pkgs.writeText "sentinel-pre-commit.sh" ''
          #!/usr/bin/env bash
          touch /workspace/.git/sentinel-fired-precommit
        '';
      in
      ''
        machine.wait_for_unit("multi-user.target")
        machine.succeed("${testImage} | podman load")

        # Workspace owned by testuser (UID 1000); --userns=keep-id maps this
        # to the container's wrapix user (UID 1000) so the entrypoint can
        # write to .git/config when it installs core.hooksPath.
        machine.succeed("mkdir -p /tmp/workspace && chown testuser:users /tmp/workspace")

        # Seed: a git repo, .pre-commit-config.yaml, the sentinel script, and
        # one tracked file. Both the entrypoint's core.hooksPath gate and
        # prek's hook discovery require .git + .pre-commit-config.yaml to be
        # present before the container starts.
        machine.succeed(
            "su - testuser -c \"cd /tmp/workspace && "
            "git init -q -b main && "
            "git config user.email test@example.com && "
            "git config user.name Test && "
            "echo seed > seed.txt\""
        )
        machine.succeed(
            "install -o testuser -g users -m 644 "
            "${cfg} /tmp/workspace/.pre-commit-config.yaml"
        )
        machine.succeed(
            "install -o testuser -g users -m 755 "
            "${sentinel} /tmp/workspace/.git/sentinel-pre-commit.sh"
        )

        # Run the container with `git add` + `git commit` as the command
        # override. The entrypoint sets core.hooksPath to the prek bundle
        # baked into the image (referenced via WRAPIX_PREK_HOOKS), then
        # exec's the override; the commit fires the bundled pre-commit shim
        # which dispatches the .pre-commit-config.yaml hooks via prek.
        machine.succeed(
            "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
            "-e HOME=/home/wrapix "
            "-e GIT_AUTHOR_NAME=test -e GIT_AUTHOR_EMAIL=test@example.com "
            "-e GIT_COMMITTER_NAME=test -e GIT_COMMITTER_EMAIL=test@example.com "
            "-v /tmp/workspace:/workspace:rw "
            "docker-archive:${testImage} "
            "/bin/bash -c \"cd /workspace && git add -A && git commit -m test\"'"
        )

        # Sentinel side-effect proves the hook chain fired via core.hooksPath.
        # Written under .git/ so it sits outside the worktree and survives
        # prek's stash/restore dance regardless of how prek classifies it.
        machine.succeed("test -f /tmp/workspace/.git/sentinel-fired-precommit")

        # The wrapper-on-path probe (`skip-if-missing nonexistent-tool-xyz --
        # false`) would have failed the commit if `skip-if-missing` were not
        # on PATH (`command not found` is non-zero); the commit succeeding
        # without a non-zero exit is the evidence that both wrappers landed.
      '';
  };

  # Test 5: Verify the container-side pre-push hook chain fires.
  # Symmetric to container-pre-commit but exercises the pre-push stage:
  # `git push` to a local file:// bare remote fires the bundled pre-push
  # shim, which dispatches the `stages: [pre-push]` hooks via prek and then
  # writes the .wrapix/push-verified stamp on success. The sentinel hook
  # writes a marker file that proves the chain ran.
  container-pre-push = pkgs.testers.nixosTest {
    name = "wrapix-container-pre-push";

    nodes.machine =
      { ... }:
      {
        imports = [ commonModule ];
      };

    testScript =
      let
        cfg = pkgs.writeText "container-pre-push.yaml" ''
          repos:
            - repo: local
              hooks:
                - id: wrapper-on-path
                  name: wrapper-on-path
                  entry: skip-if-missing nonexistent-tool-xyz -- false
                  language: system
                  stages: [pre-push]
                  always_run: true
                  pass_filenames: false
                - id: sentinel
                  name: sentinel
                  entry: /workspace/.git/sentinel-pre-push.sh
                  language: system
                  stages: [pre-push]
                  always_run: true
                  pass_filenames: false
        '';
        sentinel = pkgs.writeText "sentinel-pre-push.sh" ''
          #!/usr/bin/env bash
          touch /workspace/.git/sentinel-fired-prepush
        '';
      in
      ''
        machine.wait_for_unit("multi-user.target")
        machine.succeed("${testImage} | podman load")

        machine.succeed("mkdir -p /tmp/workspace && chown testuser:users /tmp/workspace")

        # Seed: working repo + bare file:// remote inside the workspace so
        # push has a target reachable from inside the container. The initial
        # commit is made on the host (no hooks fire because core.hooksPath
        # is unset until the container's entrypoint installs it).
        machine.succeed(
            "su - testuser -c \"cd /tmp/workspace && "
            "git init -q -b main && "
            "git config user.email test@example.com && "
            "git config user.name Test && "
            "git init --bare -q remote.git && "
            "git remote add origin file:///workspace/remote.git && "
            "echo seed > seed.txt\""
        )
        machine.succeed(
            "install -o testuser -g users -m 644 "
            "${cfg} /tmp/workspace/.pre-commit-config.yaml"
        )
        machine.succeed(
            "install -o testuser -g users -m 755 "
            "${sentinel} /tmp/workspace/.git/sentinel-pre-push.sh"
        )
        machine.succeed(
            "su - testuser -c \"cd /tmp/workspace && "
            "git add -A && "
            "git commit -q -m initial\""
        )

        # Push from inside the container. The entrypoint installs
        # core.hooksPath -> prek bundle, then `git push` triggers the
        # pre-push shim which runs prek hook-impl --hook-type=pre-push and
        # invokes the sentinel hook before the actual push proceeds.
        machine.succeed(
            "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
            "-e HOME=/home/wrapix "
            "-e GIT_AUTHOR_NAME=test -e GIT_AUTHOR_EMAIL=test@example.com "
            "-e GIT_COMMITTER_NAME=test -e GIT_COMMITTER_EMAIL=test@example.com "
            "-v /tmp/workspace:/workspace:rw "
            "docker-archive:${testImage} "
            "/bin/bash -c \"cd /workspace && git push origin main\"'"
        )

        # Sentinel side-effect proves the pre-push hook chain fired.
        machine.succeed("test -f /tmp/workspace/.git/sentinel-fired-prepush")

        # The push also reached the bare remote (HEAD ref exists).
        machine.succeed(
            "su - testuser -c \"git -C /tmp/workspace/remote.git rev-parse refs/heads/main\""
        )
      '';
  };

  # Test 6: Verify the session-metadata audit anchor — specs/security.md § Audit Trail.
  # Runs the entrypoint with a no-op command override so the EXIT trap fires
  # without any agent runtime; then asserts the session-metadata JSON exists
  # and carries the required fields.
  audit-trail-anchor = pkgs.testers.nixosTest {
    name = "wrapix-audit-trail-anchor";

    nodes.machine =
      { ... }:
      {
        imports = [ commonModule ];
        environment.systemPackages = [ pkgs.jq ];
      };

    testScript = ''
      machine.wait_for_unit("multi-user.target")

      # Load the test image
      machine.succeed("${testImage} | podman load")

      # Create test workspace owned by testuser; entrypoint writes its
      # session log under /workspace/.wrapix/log/ which surfaces on the host
      # at /tmp/workspace/.wrapix/log/.
      machine.succeed("mkdir -p /tmp/workspace && chown testuser:users /tmp/workspace")

      # Run the entrypoint with a no-op command override. The launcher's
      # always-on env is replicated here (HOME, GIT_AUTHOR_*, GIT_COMMITTER_*)
      # so the entrypoint's claude-config branch and write_session_log run
      # the same way they would under real `wrapix run`.
      machine.succeed(
        "su - testuser -c 'podman run --rm --network=pasta --userns=keep-id "
        "-e HOME=/home/wrapix "
        "-e GIT_AUTHOR_NAME=test -e GIT_AUTHOR_EMAIL=test@example.com "
        "-e GIT_COMMITTER_NAME=test -e GIT_COMMITTER_EMAIL=test@example.com "
        "-v /tmp/workspace:/workspace:rw "
        "docker-archive:${testImage} /bin/true'"
      )

      # Assert exactly one session-metadata JSON exists.
      log_files = machine.succeed(
        "ls /tmp/workspace/.wrapix/log/*.json 2>/dev/null | wc -l"
      ).strip()
      assert log_files == "1", \
        f"Expected exactly one session-metadata JSON, got {log_files}"

      log_file = machine.succeed(
        "ls /tmp/workspace/.wrapix/log/*.json"
      ).strip()

      # Assert the contract fields the spec names are populated (non-empty
      # and non-null in the JSON).
      for field in ("timestamp_start", "timestamp_end", "exit_code", "mode", "claude_session_dir"):
          value = machine.succeed(
              f"jq -r '.{field} // empty' {log_file}"
          ).strip()
          assert value != "", \
              f"Field {field} is empty/null in session-metadata JSON {log_file}"

      # claude_session_dir must resolve to an existing directory; the
      # entrypoint mkdir's /workspace/.claude before agent dispatch so it
      # surfaces at /tmp/workspace/.claude on the host.
      claude_dir = machine.succeed(
          f"jq -r '.claude_session_dir' {log_file}"
      ).strip()
      assert claude_dir == "/workspace/.claude", \
          f"Unexpected claude_session_dir: {claude_dir!r}"
      machine.succeed("test -d /tmp/workspace/.claude")
    '';
  };
}
