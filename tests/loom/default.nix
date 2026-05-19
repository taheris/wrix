{
  pkgs,
  system,
  linuxPkgs,
  fenix,
  loomPackage,
  treefmt ? null,
}:

let
  inherit (pkgs) lib;
  inherit (loomPackage) craneLib;

  shellLib = import ../../lib/util/shell.nix { };

  isLinux = lib.elem system [
    "x86_64-linux"
    "aarch64-linux"
  ];

  sandboxLib = import ../../lib/sandbox {
    inherit
      pkgs
      system
      linuxPkgs
      fenix
      treefmt
      ;
  };
  defaultImage = (sandboxLib.mkSandbox { profile = sandboxLib.profiles.base; }).image;
  piImage =
    (sandboxLib.mkSandbox {
      profile = sandboxLib.profiles.base;
      agent = "pi";
    }).image;
  defaultImageClosure = pkgs.closureInfo { rootPaths = [ defaultImage ]; };
  piImageClosure = pkgs.closureInfo { rootPaths = [ piImage ]; };
  piMonoPkg = linuxPkgs.pi-mono;
  claudeCodePkg = linuxPkgs.claude-code;

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

  # Host files referenced by [check]-tier grep annotations in
  # specs/loom-tests.md. The Nix-config invariants those annotations
  # encode (test-loom app shape, flake-checks wiring, podman/pi-mono
  # pinning) live outside the loom/ workspace, so the staged source
  # mirrors the host paths exactly under tests/, modules/flake/.
  stagedSrc = pkgs.runCommand "loom-test-src" { } ''
    cp -r ${cleanedSrc} $out
    chmod -R u+w $out
    mkdir -p $out/tests/loom $out/modules/flake
    cp -r ${../loom/mock-pi} $out/tests/loom/mock-pi
    cp -r ${../loom/mock-claude} $out/tests/loom/mock-claude
    cp ${./default.nix} $out/tests/loom/default.nix
    cp ${./run-tests.sh} $out/tests/loom/run-tests.sh
    cp ${../default.nix} $out/tests/default.nix
    cp ${../../modules/flake/apps.nix} $out/modules/flake/apps.nix
    cp ${../../modules/flake/overlays.nix} $out/modules/flake/overlays.nix
    cp -r ${../../specs} $out/specs
  '';

  nextestArgs = {
    src = stagedSrc;
    cargoLock = ../../loom/Cargo.lock;
    inherit (loomPackage) cargoArtifacts;
    nativeBuildInputs = [ pkgs.git ];
    # The nix sandbox sets HOME=/homeless-shelter (read-only) by default. The
    # lock manager resolves `$XDG_STATE_HOME/loom/locks/<basename>/` under
    # `~/.local/state` (per spec); production tests fail to create that
    # directory inside the sandbox without a writable HOME. Point both
    # workspace-test integration suites at the writable build tmpdir.
    preCheck = ''
      export HOME=$(mktemp -d)
    '';
  };

  nextestFast = craneLib.cargoNextest (
    nextestArgs
    // {
      cargoNextestExtraArgs = "-E 'not binary(properties)'";
    }
  );

  # `loom gate verify` driving the deterministic verifier tiers from
  # `specs/*.md`. Per the Nix Integration section of
  # `specs/loom-tests.md`, the derivation threads the loom binary,
  # cargo-nextest, cargoArtifacts, and staged source (with `specs/`)
  # into the build sandbox via the craneLib custom-derivation pattern.
  # LOOM_VERIFY_TIERS scopes the verify loop to `check,test` because
  # [system]-tier verifiers across the repo shell out to `nix build`,
  # `nix run`, and `podman` — none of which are available inside the
  # nix build sandbox. `--spec loom-tests` keeps the verify loop on
  # the one spec whose [check] grep targets line up with the staged
  # source layout; other specs reference paths like `loom/crates/...`
  # or `lib/sandbox/...` that the sandbox does not stage (tracked in
  # wx-1pp3u). The container smoke ([system](nix run .#test-loom))
  # stays in the separate `test-loom` app because it needs `podman`
  # at runtime.
  loomTests = craneLib.mkCargoDerivation (
    nextestArgs
    // {
      pname = "loom-tests";
      doCheck = true;
      nativeBuildInputs = nextestArgs.nativeBuildInputs ++ [
        pkgs.cargo-nextest
        loomPackage.bin
      ];
      buildPhaseCargoCommand = ''
        cargo --version
        cargo nextest --version
        loom --version
      '';
      checkPhaseCargoCommand = ''
        LOOM_VERIFY_TIERS=check,test loom gate verify --spec loom-tests
      '';
    }
  );

  propTests = craneLib.cargoNextest (
    nextestArgs
    // {
      cargoNextestExtraArgs = "-E 'binary(properties)'";
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

  # Linux-only verifier for the wrapix-spawn image-source -> podman-load
  # contract. Drives the same `imageLoadStep` snippet wrapix-spawn runs
  # through a shim podman; asserts load on first call, idempotence on the
  # second.
  wrapixSpawnLoadTest = pkgs.writeShellApplication {
    name = "test-wrapix-spawn-load";
    runtimeInputs = lib.optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text =
      if isLinux then
        ''
          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          shim_dir="$tmp/bin"
          state="$tmp/state"
          mkdir -p "$shim_dir" "$state"
          log_file="$state/podman.log"
          : >"$log_file"

          IMAGE_REF="localhost/wrapix-loadtest:abc123"
          IMAGE_SOURCE="$tmp/image-source.sh"

          # Fake image_source: emits a stream of bytes on stdout (the
          # contract `streamLayeredImage` honors). The shim discards them.
          cat >"$IMAGE_SOURCE" <<'IMG_SRC'
          #!/usr/bin/env bash
          printf 'fake-image-tarball-bytes'
          IMG_SRC
          chmod +x "$IMAGE_SOURCE"

          # Shim podman that records every call into $log_file and emulates
          # the subset the load step uses:
          #   * `image exists <ref>` returns 0 only after a load+tag has
          #     completed (state file PRESENT) — the idempotence pivot.
          #   * `load -q` accepts a tarball stream.
          #   * `tag <src> <dst>` flips the state file.
          cat >"$shim_dir/podman" <<PODMAN_SHIM
          #!/usr/bin/env bash
          set -euo pipefail
          printf '%s\n' "\$*" >>'$log_file'
          case "\$1" in
              image)
                  case "\$2" in
                      exists)
                          if [ -f '$state/loaded' ]; then exit 0; else exit 1; fi
                          ;;
                      *) exit 0 ;;
                  esac
                  ;;
              load)
                  cat >'$state/load-stdin' || true
                  exit 0
                  ;;
              tag)
                  : >'$state/loaded'
                  exit 0
                  ;;
              *) exit 0 ;;
          esac
          PODMAN_SHIM
          chmod +x "$shim_dir/podman"

          # The shared snippet expects `verbose` to be defined; production
          # wraps it around WRAPIX_VERBOSE. The test no-ops it.
          verbose() { :; }

          PATH="$shim_dir:$PATH"
          export PATH IMAGE_REF IMAGE_SOURCE

          # First invocation: the snippet must call `podman load` and
          # `podman tag <repo>:latest <ref>`.
          ${shellLib.imageLoadStep}

          if ! grep -q '^load -q' "$log_file"; then
              echo "first invocation did not call 'podman load':" >&2
              cat "$log_file" >&2
              exit 1
          fi
          if ! grep -q "^tag .*:latest $IMAGE_REF$" "$log_file"; then
              echo "first invocation did not tag image as $IMAGE_REF:" >&2
              cat "$log_file" >&2
              exit 1
          fi

          # Second invocation: the snippet must short-circuit on the cached
          # tag (no second `podman load`).
          : >"$log_file"
          ${shellLib.imageLoadStep}

          if grep -q '^load -q' "$log_file"; then
              echo "second invocation re-loaded image (load is not idempotent):" >&2
              cat "$log_file" >&2
              exit 1
          fi
          if ! grep -q "^image exists $IMAGE_REF$" "$log_file"; then
              echo "second invocation did not check 'image exists $IMAGE_REF':" >&2
              cat "$log_file" >&2
              exit 1
          fi

          echo "test-wrapix-spawn-load: PASS"
        ''
      else
        ''
          echo "test-wrapix-spawn-load: not available on Darwin (no podman dependency on macOS)" >&2
          exit 0
        '';
  };
  piRuntimeImageTest = pkgs.writeShellApplication {
    name = "test-pi-runtime-image";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      closure_file=${piImageClosure}/store-paths
      pi_mono_path=${piMonoPkg}

      if ! grep -qxF "$pi_mono_path" "$closure_file"; then
          echo "FAIL: pi-mono store path not in sandbox-pi image closure" >&2
          echo "  expected: $pi_mono_path" >&2
          echo "  closure : $closure_file" >&2
          exit 1
      fi

      if [[ ! -x "$pi_mono_path/bin/pi" ]]; then
          echo "FAIL: pi binary at $pi_mono_path/bin/pi is missing or not executable" >&2
          exit 1
      fi

      echo "test-pi-runtime-image: PASS"
    '';
  };

  claudeRuntimeNoopTest = pkgs.writeShellApplication {
    name = "test-claude-runtime-noop";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      closure_file=${defaultImageClosure}/store-paths
      pi_mono_path=${piMonoPkg}
      claude_code_path=${claudeCodePkg}

      if grep -qxF "$pi_mono_path" "$closure_file"; then
          echo "FAIL: pi-mono unexpectedly present in default sandbox closure" >&2
          echo "  found   : $pi_mono_path" >&2
          echo "  closure : $closure_file" >&2
          exit 1
      fi

      if ! grep -qxF "$claude_code_path" "$closure_file"; then
          echo "FAIL: claude-code missing from default sandbox closure" >&2
          echo "  expected: $claude_code_path" >&2
          echo "  closure : $closure_file" >&2
          exit 1
      fi

      echo "test-claude-runtime-noop: PASS"
    '';
  };
in
{
  inherit
    testLoom
    nextestFast
    loomTests
    wrapixSpawnLoadTest
    piRuntimeImageTest
    claudeRuntimeNoopTest
    ;
}
