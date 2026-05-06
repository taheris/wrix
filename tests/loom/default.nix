# Loom container smoke runner — Linux only.
#
# Unit + integration coverage now comes from `loom-clippy` and `loom-nextest`
# wired in `tests/default.nix` from `wrapix.loomPackage` (the rust profile's
# buildPackage outputs); this file is just the podman-bound smoke runner that
# `nix run .#test-loom` resolves to. Excluded from `flake check` because it
# needs podman at runtime.
{
  pkgs,
  system,
  linuxPkgs,
  fenix,
  treefmt ? null,
}:

let
  inherit (pkgs) lib;

  isLinux = lib.elem system [
    "x86_64-linux"
    "aarch64-linux"
  ];

  # Container smoke — Linux only. Uses a dedicated wrapix sandbox built with
  # `agent = "pi"` so the image carries pi-mono; the harness then prepends
  # an in-workspace mock-pi shim to PATH so the entrypoint's `pi --mode rpc`
  # resolves to the mock instead of the real binary.
  loomSmoke =
    if !isLinux then
      null
    else
      let
        sandbox = import ../../lib/sandbox {
          inherit
            pkgs
            system
            linuxPkgs
            fenix
            treefmt
            ;
        };

        # The sandbox under test: a profile with a unique name (so its
        # image tag does not collide with the developer's cached
        # localhost/wrapix-base:* claude image — the launcher's IMAGE_ID
        # is profile-name-keyed, not agent-keyed) and `agent = "pi"` so
        # the entrypoint takes the `pi --mode rpc` branch. The harness
        # then prepends an in-workspace mock-pi shim to PATH so `pi`
        # resolves to the mock instead of the real binary.
        smokeProfile = sandbox.profiles.base // {
          name = "loom-smoke";
        };

        smokeSandbox = sandbox.mkSandbox {
          profile = smokeProfile;
          agent = "pi";
        };

        # The streamLayeredImage publishes the image under the agent-suffixed
        # name `wrapix-<profile>-<agent>:latest` (see lib/sandbox/image.nix).
        # The launcher's `podman tag wrapix-<profile>:latest <hashed>` retag
        # silently fails for the agent variant (the source tag doesn't exist
        # without the suffix), so wiring `SpawnConfig.image` to the
        # agent-suffixed `:latest` tag is the path that survives the wrapix
        # launcher's image-resolution quirk on the first run.
        smokeImageRef = "localhost/wrapix-${smokeSandbox.profile.name}-pi:latest";

        # Claude variant: same shape, but `agent = "claude"` so the image
        # carries the claude binary and the entrypoint takes the
        # WRAPIX_AGENT=claude && WRAPIX_STDIO=1 stream-json branch.
        claudeSmokeProfile = sandbox.profiles.base // {
          name = "loom-claude-smoke";
        };

        claudeSmokeSandbox = sandbox.mkSandbox {
          profile = claudeSmokeProfile;
          agent = "claude";
        };

        claudeSmokeImageRef = "localhost/wrapix-${claudeSmokeSandbox.profile.name}-claude:latest";

        beadsModule = import ../../lib/beads { inherit pkgs linuxPkgs; };
      in
      pkgs.writeShellApplication {
        name = "test-loom";
        runtimeInputs = [
          smokeSandbox.package
          claudeSmokeSandbox.package
          pkgs.beads
          beadsModule.dolt
          pkgs.dolt
          pkgs.git
          pkgs.jq
          pkgs.podman
          pkgs.coreutils
        ];
        text = ''
          export WRAPIX_LOOM_MOCK_PI_SCRIPT=${../loom/mock-pi/pi.sh}
          export WRAPIX_LOOM_MOCK_CLAUDE_SCRIPT=${../loom/mock-claude/claude.sh}
          export WRAPIX_LOOM_WRAPIX_BIN=${smokeSandbox.package}/bin/wrapix
          export WRAPIX_LOOM_TEST_IMAGE_REF=${lib.escapeShellArg smokeImageRef}
          export WRAPIX_LOOM_TEST_IMAGE_SOURCE=${smokeSandbox.image}
          ${./run-tests.sh}

          # Claude smoke uses its own wrapix package + image (the launcher
          # binary is per-sandbox, baked from mkSandbox, so we swap both).
          export WRAPIX_LOOM_WRAPIX_BIN=${claudeSmokeSandbox.package}/bin/wrapix
          export WRAPIX_LOOM_TEST_IMAGE_REF=${lib.escapeShellArg claudeSmokeImageRef}
          export WRAPIX_LOOM_TEST_IMAGE_SOURCE=${claudeSmokeSandbox.image}
          exec ${./run-claude-tests.sh}
        '';
      };

  # Darwin no-op — `nix run .#test-loom` resolves here on macOS and prints
  # a clear skip message to stderr (NFR #7). Exit 0 keeps the smoke off
  # the cross-platform critical path without needing platform-specific
  # CI gates.
  loomSmokeDarwinSkip = pkgs.writeShellApplication {
    name = "test-loom";
    text = ''
      echo "container smoke not available on Darwin (no podman dependency on macOS)" >&2
      exit 0
    '';
  };

in
{
  inherit loomSmoke loomSmokeDarwinSkip;
}
