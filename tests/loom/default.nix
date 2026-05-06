{
  pkgs,
  system,
  linuxPkgs,
  fenix,
  loomPackage,
  rustProfile,
  treefmt ? null,
}:

let
  inherit (pkgs) lib;
  inherit (rustProfile) craneLib;

  isLinux = lib.elem system [
    "x86_64-linux"
    "aarch64-linux"
  ];

  # Mirrors lib/loom/default.nix's filter so the locally-built nextest
  # variants see the same src as `loomPackage.nextest`.
  srcFilter =
    path: type:
    (craneLib.filterCargoSources path type)
    || (lib.hasInfix "/loom-templates/templates/" path)
    || (lib.hasSuffix ".snap" path);

  cleanedSrc = lib.cleanSourceWith {
    src = ../../loom;
    filter = srcFilter;
  };

  stagedSrc = pkgs.runCommand "loom-test-src" { } ''
    cp -r ${cleanedSrc} $out
    chmod -R u+w $out
    mkdir -p $out/tests/loom
    cp -r ${../loom/mock-pi} $out/tests/loom/mock-pi
    cp -r ${../loom/mock-claude} $out/tests/loom/mock-claude
    cp ${../loom-test.sh} $out/tests/loom-test.sh
    cp -r ${../../specs} $out/specs
  '';

  nextestArgs = {
    src = stagedSrc;
    cargoLock = ../../loom/Cargo.lock;
    inherit (loomPackage) cargoArtifacts;
    nativeBuildInputs = [ pkgs.git ];
  };

  nextestFast = craneLib.cargoNextest (
    nextestArgs
    // {
      cargoNextestExtraArgs = "-E 'not binary(properties)'";
    }
  );

  propTests = craneLib.cargoNextest (
    nextestArgs
    // {
      cargoNextestExtraArgs = "-E 'binary(properties)'";
      PROPTEST_CASES = "512";
    }
  );

  smoke =
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

      piProfile = sandbox.profiles.base // {
        name = "loom-smoke";
      };
      piSandbox = sandbox.mkSandbox {
        profile = piProfile;
        agent = "pi";
      };
      piImageRef = "localhost/wrapix-${piSandbox.profile.name}-pi:latest";

      claudeProfile = sandbox.profiles.base // {
        name = "loom-claude-smoke";
      };
      claudeSandbox = sandbox.mkSandbox {
        profile = claudeProfile;
        agent = "claude";
      };
      claudeImageRef = "localhost/wrapix-${claudeSandbox.profile.name}-claude:latest";

      beadsModule = import ../../lib/beads { inherit pkgs linuxPkgs; };
    in
    {
      runtimeInputs = [
        beadsModule.dolt
        claudeSandbox.package
        piSandbox.package
        pkgs.beads
        pkgs.coreutils
        pkgs.dolt
        pkgs.git
        pkgs.jq
        pkgs.podman
      ];
      text = ''
        export WRAPIX_LOOM_MOCK_PI_SCRIPT=${../loom/mock-pi/pi.sh}
        export WRAPIX_LOOM_MOCK_CLAUDE_SCRIPT=${../loom/mock-claude/claude.sh}
        export WRAPIX_LOOM_WRAPIX_BIN=${piSandbox.package}/bin/wrapix
        export WRAPIX_LOOM_TEST_IMAGE_REF=${lib.escapeShellArg piImageRef}
        export WRAPIX_LOOM_TEST_IMAGE_SOURCE=${piSandbox.image}
        ${./run-tests.sh}

        export WRAPIX_LOOM_WRAPIX_BIN=${claudeSandbox.package}/bin/wrapix
        export WRAPIX_LOOM_TEST_IMAGE_REF=${lib.escapeShellArg claudeImageRef}
        export WRAPIX_LOOM_TEST_IMAGE_SOURCE=${claudeSandbox.image}
        exec ${./run-claude-tests.sh}
      '';
    };

  darwinSkip = ''echo "container smoke not available on Darwin (no podman dependency on macOS)" >&2'';

  testLoom = pkgs.writeShellApplication {
    name = "test-loom";
    runtimeInputs = lib.optionals isLinux smoke.runtimeInputs;
    text = ''
      echo "loom property tests: ${propTests}"
      ${if isLinux then smoke.text else darwinSkip}
    '';
  };
in
{
  inherit testLoom nextestFast;
}
