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

  controllerRuntime = pkgs.symlinkJoin {
    name = "wrix-controller-runtime";
    paths = [
      pkgs.nix
      pkgs.skopeo
      pkgs.openssl
    ]
    ++ optionals pkgs.stdenv.hostPlatform.isLinux [
      pkgs.systemd
      pkgs.podman
      (import ../sandbox/linux/krun-runtime.nix { linuxPkgs = pkgs; })
    ];
  };
  controllerPackage =
    pkgs.runCommand "wrix-host-controller"
      {
        nativeBuildInputs = [ pkgs.makeWrapper ];
      }
      ''
        set -euo pipefail

        mkdir -p "$out/bin"
        makeWrapper "${workspace.bin}/bin/wrix-host-controller" "$out/bin/wrix-host-controller" \
          --prefix PATH : "${controllerRuntime}/bin"
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
      "${makeBinPath [
        pkgs.nix
        pkgs.skopeo
      ]}"
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
      runHook preBuild
      cargo test --offline --locked --no-run -p wrix-cache --test celler_backend \
        ''${CARGO_PROFILE:+--profile "$CARGO_PROFILE"} --message-format json > compiled-tests.json
      cargo test --offline --locked --no-run -p wrix-service --test celler_auth \
        ''${CARGO_PROFILE:+--profile "$CARGO_PROFILE"} --message-format json >> compiled-tests.json
      runHook postBuild
    '';
    installPhase = ''
      for target in celler_backend celler_auth; do
        executable=$(${pkgs.jq}/bin/jq -r --arg target "$target" \
          'select(.reason == "compiler-artifact" and .target.name == $target and .profile.test) | .executable' \
          compiled-tests.json)
        test -n "$executable"
        install -Dm755 "$executable" "$out/bin/''${target//_/-}-tests"
      done
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
  inherit controllerBinding controllerPackage;
  focusNotification = mkBinaryPackage "wrix-focus-notification";
  celler = cellerPackage;
  supervisor =
    pkgs.runCommand "wrix-service-supervisor" { meta = binaryMeta "wrix-service-supervisor"; }
      ''
        install -Dm755 "${workspace.bin}/bin/wrix-service-supervisor" "$out/bin/wrix-service-supervisor"
      '';
  cacheHook = mkBinaryPackage "wrix-cache-hook";
  cachePublish = mkBinaryPackage "wrix-cache-publish";
}
