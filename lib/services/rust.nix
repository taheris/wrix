{ pkgs, rustProfile }:

let
  inherit (builtins) concatStringsSep;

  workspace = rustProfile.buildPackage {
    src = ../..;
    cargoLock = ../../Cargo.lock;
    extraSrcs = {
      "crates/wrix-sandbox/tests/fixtures/consumer-entrypoint.sh" =
        ../../crates/wrix-sandbox/tests/fixtures/consumer-entrypoint.sh;
      "crates/wrix-sandbox/tests/fixtures/container-spawn-runtime.sh" =
        ../../crates/wrix-sandbox/tests/fixtures/container-spawn-runtime.sh;
      "crates/wrix-sandbox/tests/fixtures/podman-spawn-runtime.sh" =
        ../../crates/wrix-sandbox/tests/fixtures/podman-spawn-runtime.sh;
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
    printf 'args='
    printf '%s ' "$@"
    printf '\n'
  '';

  prekHooksBundle = import ../prek/bundle.nix { inherit pkgs; };
  prekRunner = import ../prek/runner.nix { inherit pkgs; };

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
      WRIX_TEST_PUBLISHER_HELPER = "${cacheHookTestPublisher}/bin/wrix-cache-test-publisher";
    }
  );

  package = workspace.bin;
  wrix = mkWrappedBinaryPackage "wrix" [
    "--set"
    "WRIX_PREK_HOOKS"
    "${prekHooksBundle}"
    "--prefix"
    "PATH"
    ":"
    "${pkgs.lib.makeBinPath [
      pkgs.coreutils
      pkgs.dolt
    ]}"
  ];
  cacheHook = mkBinaryPackage "wrix-cache-hook";
  cachePublish = mkBinaryPackage "wrix-cache-publish";
  cacheServe = mkBinaryPackage "wrix-cache-serve";
}
