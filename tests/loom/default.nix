# Loom unit + integration tests as a `nix flake check` gate, plus the
# Linux-only container smoke runner.
#
# Per specs/loom-tests.md (Architecture / Nix Integration):
#   - `loom-tests`  — runs `cargo nextest run --workspace` under the build
#                     sandbox. Cross-platform (NFR #7).
#   - `loom-smoke`  — Linux-only `writeShellApplication` exposed as
#                     `nix run .#test-loom`. Excluded from `flake check`
#                     because it needs podman at runtime.
#
# Reuses the same fenix-pinned toolchain as lib/loom/default.nix so tests
# run against the same compiler the production binary is built with.
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

  fenixPkgs =
    if fenix == null then
      throw "tests/loom/default.nix: requires the fenix input"
    else
      fenix.packages.${pkgs.stdenv.hostPlatform.system};

  toolchain = fenixPkgs.stable.defaultToolchain;

  rustPlatform = pkgs.makeRustPlatform {
    cargo = toolchain;
    rustc = toolchain;
  };

  loomTests = rustPlatform.buildRustPackage {
    pname = "loom-tests";
    version = "0.1.0";
    src = ../../loom;

    cargoLock = {
      lockFile = ../../loom/Cargo.lock;
    };

    env = {
      HOME = "/tmp";
      # Per spec NFR §Property-Based Testing — proptest case count for CI.
      # Local exhaustive runs override via `PROPTEST_CASES=2048+`.
      PROPTEST_CASES = "32";
    };

    # Mirror lib/loom/default.nix: fixtures live outside the loom workspace
    # but are referenced from integration tests via CARGO_MANIFEST_DIR-relative
    # paths. Stage them next to the unpacked source.
    postUnpack = ''
      mkdir -p tests/loom
      cp -r ${../../tests/loom/mock-pi} tests/loom/mock-pi
      cp -r ${../../tests/loom/mock-claude} tests/loom/mock-claude
      chmod -R u+w tests/loom
    '';

    useNextest = true;
    nativeCheckInputs = [ pkgs.git ];
    cargoTestFlags = [ "--workspace" ];
    doCheck = true;

    # This derivation exists for its tests; the binary is built and
    # installed by lib/loom/default.nix. Drop the install step and emit a
    # passing-marker so the check has an output path.
    installPhase = ''
      runHook preInstall
      mkdir -p $out
      runHook postInstall
    '';

    meta = {
      description = "Loom unit + integration tests (cargo nextest --workspace)";
    };
  };

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

        beadsModule = import ../../lib/beads { inherit pkgs linuxPkgs; };
      in
      pkgs.writeShellApplication {
        name = "test-loom";
        runtimeInputs = [
          smokeSandbox.package
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
          export WRAPIX_LOOM_WRAPIX_BIN=${smokeSandbox.package}/bin/wrapix
          export WRAPIX_LOOM_TEST_IMAGE_REF=${lib.escapeShellArg smokeImageRef}
          exec ${./run-tests.sh} "$@"
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
  inherit loomTests loomSmoke loomSmokeDarwinSkip;

  # `loom-tests` is the cargo nextest gate consumed by `tests/default.nix`'s
  # checks set; cross-platform per NFR #7.
  loom-tests = loomTests;
}
