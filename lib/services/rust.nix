{
  pkgs,
  rustProfile,
  prekDevShellHook ? null,
  serviceImage ? null,
  nixPoolImage ? null,
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
      "lib/services/host-nix-config.sh" = ./host-nix-config.sh;
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

  poolBinding = import ./binding.nix {
    inherit pkgs;
    image = nixPoolImage;
  };

  controllerRuntime = pkgs.symlinkJoin {
    name = "wrix-controller-runtime";
    paths = [
      pkgs.nix
      pkgs.skopeo
      pkgs.git
      pkgs.openssl
      pkgs.openssh
    ]
    ++ optionals pkgs.stdenv.hostPlatform.isLinux [
      pkgs.systemd
      pkgs.podman
      pkgs.sshfs
      (import ../sandbox/linux/krun-runtime.nix { linuxPkgs = pkgs; })
    ];
  };
  poolHostTools = pkgs.writeText "wrix-nix-pool-host-tools.json" (
    builtins.toJSON {
      version = 1;
      root = toString controllerRuntime;
      system = pkgs.stdenv.hostPlatform.system;
    }
  );
  controllerPackage =
    pkgs.runCommand "wrix-host-controller"
      {
        nativeBuildInputs = [ pkgs.makeWrapper ];
      }
      ''
        set -euo pipefail

        mkdir -p "$out/bin"
        makeWrapper "${workspace.bin}/bin/wrix-host-controller" "$out/bin/wrix-host-controller" \
          --prefix PATH : "${controllerRuntime}/bin" ${
            if nixPoolImage == null then
              ""
            else
              ''--set WRIX_NIX_POOL_PACKAGE_BINDING "${poolBinding}" ${
                if pkgs.stdenv.hostPlatform.isLinux then
                  ''--set WRIX_NIX_POOL_HOST_TOOLS "${poolHostTools}"''
                else
                  ""
              }''
          }
      '';
  controllerBinding = import ./control-binding.nix {
    inherit pkgs;
    controller = controllerPackage;
    runtime = controllerRuntime;
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
        pkgs.git
        pkgs.dolt
        pkgs.openssl
      ]}"
    ]
    ++ optionals (nixPoolImage != null) [
      "--set"
      "WRIX_NIX_POOL_PACKAGE_BINDING"
      "${poolBinding}"
    ]
    ++ optionals (nixPoolImage != null && pkgs.stdenv.hostPlatform.isLinux) [
      "--set"
      "WRIX_NIX_POOL_HOST_TOOLS"
      "${poolHostTools}"
    ]
    ++ optionals (serviceImage != null) [
      "--set"
      "WRIX_SERVICE_PACKAGE_BINDING"
      "${serviceBinding}"
      "--set"
      "WRIX_CONTROLLER_PACKAGE_BINDING"
      "${controllerBinding}"
      "--prefix"
      "PATH"
      ":"
      "${makeBinPath (
        [
          pkgs.nix
          pkgs.skopeo
        ]
        ++ optionals pkgs.stdenv.hostPlatform.isLinux [
          pkgs.podman
          (import ../sandbox/linux/krun-runtime.nix { linuxPkgs = pkgs; })
        ]
      )}"
    ]
  );

  cellerPackage = import ./celler.nix {
    inherit pkgs;
    inherit (rustProfile) craneLib;
  };

  fixtures = import ../../tests/prek/native-fixtures.nix {
    inherit pkgs;
    celler = cellerPackage;
    packagedWrix = wrixPackage;
    controller = controllerPackage;
    inherit serviceImage;
    devshellHook = prekDevShellHook;
  };

  binaryMeta = name: {
    description = "Rust ${name} binary";
    mainProgram = name;
  };

  mkBinaryPackage =
    name: runtime:
    pkgs.runCommand name
      {
        meta = binaryMeta name;
        nativeBuildInputs = [ pkgs.makeWrapper ];
      }
      ''
        mkdir -p "$out/bin"
        ${
          if runtime == [ ] then
            ''ln -s "${workspace.bin}/bin/${name}" "$out/bin/${name}"''
          else
            ''makeWrapper "${workspace.bin}/bin/${name}" "$out/bin/${name}" --prefix PATH : "${makeBinPath runtime}"''
        }
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
        makeWrapper "${workspace.bin}/bin/${name}" "$out/bin/${name}" \
          --set WRIX_INVOKED_BIN "$out/bin/${name}" ${concatStringsSep " " wrapperArgs}
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
      nativeBuildInputs =
        nativeBuildInputs
        ++ [
          pkgs.beads
          pkgs.dolt
          pkgs.hostname
          pkgs.jq
          pkgs.nix
          pkgs.openssh
          pkgs.openssl
          pkgs.python3
          pkgs.tmux
        ]
        ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.util-linux ];
      preCheck = preCheck + ''
        HOME="$(mktemp -d)"
        export HOME
      '';
      WRIX_TEST_PUBLISHER_HELPER = "${cacheHookTestPublisher}/bin/wrix-cache-test-publisher";
    }
    // fixtures.environment
  );

  cacheBackendTests = workspace.nextest.overrideAttrs {
    pname = "wrix-celler-backend-tests";
    doCheck = false;
    doInstallCargoArtifacts = false;
    buildPhase = ''
      export CARGO_TARGET_DIR="$out/target"
      runHook preBuild
      : > compiled-tests.json
      for target in celler_backend host_publication_only host_signed_exclusion; do
        cargo test --offline --locked --no-run -p wrix-cache --test "$target" \
          ''${CARGO_PROFILE:+--profile "$CARGO_PROFILE"} --message-format json >> compiled-tests.json
      done
      cargo test --offline --locked --no-run -p wrix-service --test celler_auth \
        ''${CARGO_PROFILE:+--profile "$CARGO_PROFILE"} --message-format json >> compiled-tests.json
      for target in service_lifecycle workspace_admission; do
        cargo test --offline --locked --no-run -p wrix-cli --test "$target" \
          ''${CARGO_PROFILE:+--profile "$CARGO_PROFILE"} --message-format json >> compiled-tests.json
      done
      cargo test --offline --locked --no-run -p wrix-sandbox --test execution_lifetime \
        ''${CARGO_PROFILE:+--profile "$CARGO_PROFILE"} --message-format json >> compiled-tests.json
      runHook postBuild
    '';
    installPhase = ''
      for target in celler_backend celler_auth host_publication_only host_signed_exclusion service_lifecycle workspace_admission execution_lifetime; do
        executable=$(${pkgs.jq}/bin/jq -r --arg target "$target" \
          'select(.reason == "compiler-artifact" and .target.name == $target and .profile.test) | .executable' \
          compiled-tests.json)
        test -n "$executable"
        install -Dm755 "$executable" "$out/bin/''${target//_/-}-tests"
      done
      # Cargo embeds binary paths into CLI tests. Keep only those executables at
      # their immutable compilation paths, not dependency/incremental artifacts.
      mkdir -p "$TMPDIR/native-test-binaries"
      ${pkgs.jq}/bin/jq -r 'select(.reason == "compiler-artifact" and .profile.test == false and .executable != null) | .executable' compiled-tests.json | sort -u > binaries
      while IFS= read -r executable; do
        relative="''${executable#"$CARGO_TARGET_DIR"/}"
        mkdir -p "$TMPDIR/native-test-binaries/$(dirname "$relative")"
        cp "$executable" "$TMPDIR/native-test-binaries/$relative"
      done < binaries
      rm -rf "$CARGO_TARGET_DIR"
      mkdir -p "$CARGO_TARGET_DIR"
      cp -r "$TMPDIR/native-test-binaries/." "$CARGO_TARGET_DIR/"
      "$out/bin/celler-backend-tests" --list --ignored > "$out/ignored-tests"
      grep -Fx 'publisher_reuses_rust_store_client: test' "$out/ignored-tests"
      grep -Fx 'production_runtime_excludes_competing_owners_until_process_exit: test' "$out/ignored-tests"
      "$out/bin/celler-auth-tests" --list | grep -Fx \
        'withdrawn_private_policy_denies_renewal_but_old_native_grants_read_new_publications: test'
    '';
  };

  testFixtures = fixtures.bundle;
  package = workspace.bin;
  wrix = wrixPackage;
  binding = serviceBinding;
  inherit
    controllerBinding
    controllerPackage
    poolBinding
    poolHostTools
    ;
  focusNotification = mkBinaryPackage "wrix-focus-notification" [ ];
  celler = cellerPackage;
  supervisor =
    pkgs.runCommand "wrix-service-supervisor" { meta = binaryMeta "wrix-service-supervisor"; }
      ''
        install -Dm755 "${workspace.bin}/bin/wrix-service-supervisor" "$out/bin/wrix-service-supervisor"
        install -Dm755 "${workspace.bin}/bin/wrix-shared-service" "$out/bin/wrix-shared-service"
      '';
  nixPoolOwner = pkgs.runCommand "wrix-nix-pool-owner" { meta = binaryMeta "wrix-nix-pool-owner"; } ''
    install -Dm755 "${workspace.bin}/bin/wrix-nix-pool-owner" "$out/bin/wrix-nix-pool-owner"
  '';
  hostCachePolicy = mkBinaryPackage "wrix-host-cache-policy" [ ];
  cacheHook = mkBinaryPackage "wrix-cache-hook" [ ];
  cachePublish = mkBinaryPackage "wrix-cache-publish" [
    pkgs.git
    pkgs.nix
  ];
}
