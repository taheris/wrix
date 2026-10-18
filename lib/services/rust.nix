{
  pkgs,
  rustProfile,
  prekDevShellHook ? null,
  serviceImage ? null,
}:

let
  inherit (builtins) concatStringsSep;
  inherit (pkgs.lib) makeBinPath removePrefix optionals;

  workspace = rustProfile.buildPackage {
    src = ../..;
    cargoLock = ../../Cargo.lock;
    srcFilter =
      path: type:
      let
        relative = removePrefix "${toString ../..}/" path;
      in
      builtins.match "crates/[^/]+/tests(/.*)?" relative == null
      && rustProfile.craneLib.filterCargoSources path type;
    extraSrcs = {
      "crates/wrix-cache/tests" = ../../crates/wrix-cache/tests;
      "crates/wrix-cli/tests" = ../../crates/wrix-cli/tests;
      "crates/wrix-core/tests" = ../../crates/wrix-core/tests;
      "crates/wrix-notify/tests" = ../../crates/wrix-notify/tests;
      "crates/wrix-sandbox/tests" = ../../crates/wrix-sandbox/tests;
      "crates/wrix-service/tests" = ../../crates/wrix-service/tests;
      ".config/nextest.toml" = ../../.config/nextest.toml;
      "tests/prepare-native-fixtures.sh" = ../../tests/prepare-native-fixtures.sh;
      "lib/util/git-ssh-setup.sh" = ../util/git-ssh-setup.sh;
      "lib/sandbox/network-bootstrap.sh" = ../sandbox/network-bootstrap.sh;
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

  serviceBinding = import ./binding.nix {
    inherit pkgs;
    image = serviceImage;
  };

  wrixPackage = mkWrappedBinaryPackage "wrix" (
    [
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
    ]
    ++ optionals (serviceImage != null) [
      "--set"
      "WRIX_SERVICE_PACKAGE_BINDING"
      "${serviceBinding}"
      "--prefix"
      "PATH"
      ":"
      "${makeBinPath [
        pkgs.nix
        pkgs.skopeo
      ]}"
    ]
  );

  fixtures = import ../../tests/prek/native-fixtures.nix {
    inherit pkgs;
    packagedWrix = wrixPackage;
    devshellHook = prekDevShellHook;
  };

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
        pkgs.nix
        pkgs.openssh
        pkgs.python3
        pkgs.tmux
      ];
      preCheck = preCheck + ''
        HOME="$(mktemp -d)"
        export HOME
      '';
      WRIX_TEST_PUBLISHER_HELPER = "${cacheHookTestPublisher}/bin/wrix-cache-test-publisher";
    }
    // fixtures.environment
  );

  testFixtures = fixtures.bundle;
  package = workspace.bin;
  wrix = wrixPackage;
  binding = serviceBinding;
  focusNotification = mkBinaryPackage "wrix-focus-notification";
  cacheHook = mkBinaryPackage "wrix-cache-hook";
  cachePublish = mkBinaryPackage "wrix-cache-publish";
  cacheServe = mkBinaryPackage "wrix-cache-serve";
}
