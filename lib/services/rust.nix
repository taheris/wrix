{
  pkgs,
  rustProfile,
  prekDevShellHook ? null,
}:

let
  inherit (builtins) concatStringsSep;
  inherit (pkgs.lib) makeBinPath optionalAttrs;

  workspace = rustProfile.buildPackage {
    src = ../..;
    cargoLock = ../../Cargo.lock;
    extraSrcs = {
      "crates/wrix-cli/tests/fixtures/launch-runtime.sh" =
        ../../crates/wrix-cli/tests/fixtures/launch-runtime.sh;
      "crates/wrix-cli/tests/snapshots/init_help.txt" =
        ../../crates/wrix-cli/tests/snapshots/init_help.txt;
      "crates/wrix-cli/tests/snapshots/run_help.txt" = ../../crates/wrix-cli/tests/snapshots/run_help.txt;
      "crates/wrix-sandbox/tests/fixtures/consumer-entrypoint.sh" =
        ../../crates/wrix-sandbox/tests/fixtures/consumer-entrypoint.sh;
      "crates/wrix-sandbox/tests/fixtures/container-spawn-runtime.sh" =
        ../../crates/wrix-sandbox/tests/fixtures/container-spawn-runtime.sh;
      "crates/wrix-sandbox/tests/fixtures/lifecycle-runtime.sh" =
        ../../crates/wrix-sandbox/tests/fixtures/lifecycle-runtime.sh;
      "crates/wrix-sandbox/tests/fixtures/podman-spawn-runtime.sh" =
        ../../crates/wrix-sandbox/tests/fixtures/podman-spawn-runtime.sh;
      "lib/util/git-ssh-setup.sh" = ../util/git-ssh-setup.sh;
      "tests/standalone/notify-runtime.sh" = ../../tests/standalone/notify-runtime.sh;
    };
    nativeBuildInputs = [ pkgs.git ];

    meta = {
      description = "Rust wrix service and cache CLI foundation";
      mainProgram = "wrix";
    };
  };

  cacheHookTestPublisher = pkgs.writeShellScriptBin "wrix-cache-test-publisher" ''
    set -euo pipefail

    printf 'uid=%s\n' "$UID"
    printf 'gid=%s\n' "$(${pkgs.coreutils}/bin/id -g)"
    printf 'args='
    printf '%s ' "$@"
    printf '\n'
  '';

  prekHooksBundle = import ../prek/bundle.nix { inherit pkgs; };
  prekRunner = import ../prek/runner.nix { inherit pkgs; };

  wrixPackage = mkWrappedBinaryPackage "wrix" [
    "--set"
    "WRIX_PREK_HOOKS"
    "${prekHooksBundle}"
    "--set"
    "WRIX_PREK_RUNNER"
    "${prekRunner}/bin/wrix-prek"
    "--prefix"
    "PATH"
    ":"
    "${makeBinPath [
      pkgs.coreutils
      pkgs.dolt
    ]}"
  ];

  binaryMeta = name: {
    description = "Rust ${name} binary";
    mainProgram = name;
  };

  mkBinaryPackage =
    name:
    pkgs.runCommand name
      {
        meta = binaryMeta name;
      }
      ''
        mkdir -p "$out/bin"
        ln -s "${workspace.bin}/bin/${name}" "$out/bin/${name}"
      '';

  mkWrappedBinaryPackage =
    name: wrapperArgs:
    pkgs.runCommand name
      {
        nativeBuildInputs = [ pkgs.makeWrapper ];
        meta = binaryMeta name;
      }
      ''
        mkdir -p "$out/bin"
        makeWrapper "${workspace.bin}/bin/${name}" "$out/bin/${name}" ${concatStringsSep " " wrapperArgs}
        ln -s "${workspace.bin}/bin/wrix-git-sign" "$out/bin/wrix-git-sign"
        ln -s "${prekRunner}/bin/wrix-prek" "$out/bin/wrix-prek"
      '';

in
{
  inherit (workspace) cargoArtifacts clippy;

  nextest = workspace.nextest.overrideAttrs (
    {
      nativeBuildInputs ? [ ],
      preCheck ? "",
      ...
    }:
    {
      nativeBuildInputs = nativeBuildInputs ++ [
        pkgs.beads
        pkgs.dolt
        pkgs.hostname
        pkgs.jq
        pkgs.openssh
        pkgs.tmux
      ];
      preCheck = preCheck + ''
        HOME="$(mktemp -d)"
        export HOME
      '';
      WRIX_TEST_PUBLISHER_HELPER = "${cacheHookTestPublisher}/bin/wrix-cache-test-publisher";
      WRIX_TEST_PREK_HOOKS = "${prekHooksBundle}";
      WRIX_TEST_PREK_RUNNER = "${prekRunner}/bin/wrix-prek";
      WRIX_TEST_BROKEN_PREK_RUNNER = "${
        import ../../tests/prek/missing-runtime.nix { inherit pkgs; }
      }/bin/wrix-prek";
      WRIX_TEST_PREK_BUILD_SPEC = import ../../tests/prek/bundle-build-spec.nix { inherit pkgs; };
      WRIX_TEST_PACKAGED_WRIX = "${wrixPackage}/bin/wrix";
    }
    // optionalAttrs (prekDevShellHook != null) {
      WRIX_TEST_DEVSHELL_HOOK = pkgs.writeText "wrix-test-devshell-hook" prekDevShellHook;
    }
  );

  package = workspace.bin;
  wrix = wrixPackage;
  cacheHook = mkBinaryPackage "wrix-cache-hook";
  cachePublish = mkBinaryPackage "wrix-cache-publish";
  cacheServe = mkBinaryPackage "wrix-cache-serve";
}
