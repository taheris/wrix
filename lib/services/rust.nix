{
  pkgs,
  rustProfile,
  prekDevShellHook ? null,
  serviceImage ? null,
  nixPoolImage ? null,
}:

let
  inherit (builtins) concatStringsSep;
  inherit (pkgs) gitMinimal;
  inherit (pkgs.lib) makeBinPath optionals;

  exampleCrates = builtins.filter (name: builtins.pathExists (../../crates + "/${name}/examples")) (
    builtins.attrNames (builtins.readDir ../../crates)
  );
  exampleSources = builtins.listToAttrs (
    map (name: {
      name = "crates/${name}/examples";
      value = ../../crates + "/${name}/examples";
    }) exampleCrates
  );

  workspaceArgs = {
    src = ../..;
    cargoLock = ../../Cargo.lock;
    srcFilter = import ./rust-source-filter.nix {
      inherit (pkgs) lib;
      inherit (rustProfile.craneLib) filterCargoSources;
      root = ../..;
    };
    extraSrcs = {
      "crates/wrix-cache/tests" = ../../crates/wrix-cache/tests;
      "crates/wrix-cli/tests" = ../../crates/wrix-cli/tests;
      "crates/wrix-core/tests" = ../../crates/wrix-core/tests;
      "crates/wrix-image-tools/tests" = ../../crates/wrix-image-tools/tests;
      "crates/wrix-notify/tests" = ../../crates/wrix-notify/tests;
      "crates/wrix-sandbox/tests" = ../../crates/wrix-sandbox/tests;
      "crates/wrix-service/tests" = ../../crates/wrix-service/tests;
      "crates/wrix-verify-tools/tests" = ../../crates/wrix-verify-tools/tests;
      ".config/nextest.toml" = ../../.config/nextest.toml;
      "loom.toml" = ../../loom.toml;
      "tests" = ../../tests;
      "lib/util/git-ssh-setup.sh" = ../util/git-ssh-setup.sh;
      "lib/sandbox/install-known-hosts.sh" = ../sandbox/install-known-hosts.sh;
      # Native initializer and OCI installation tests execute these exact resources.
      "lib/sandbox/initialize-private-nix.sh" = ../sandbox/initialize-private-nix.sh;
      "lib/sandbox/oci-layout-build.sh" = ../sandbox/oci-layout-build.sh;
      "lib/beads/package.nix" = ../beads/package.nix;
      "lib/services/host-nix-config.sh" = ./host-nix-config.sh;
      "lib/services/nix-pool/bootstrap-process.sh" = ./nix-pool/bootstrap-process.sh;
      "lib/sandbox/linux/entrypoint.sh" = ../sandbox/linux/entrypoint.sh;
      "lib/sandbox/darwin/entrypoint.sh" = ../sandbox/darwin/entrypoint.sh;
      "lib/sandbox/network-ready.sh" = ../sandbox/network-ready.sh;
      "lib/sandbox/mcp-manifest.sh" = ../sandbox/mcp-manifest.sh;
      "lib/sandbox/shared-nix-client.sh" = ../sandbox/shared-nix-client.sh;
      "lib/sandbox/tool-cache.sh" = ../sandbox/tool-cache.sh;
      "lib/beads/sandbox.sh" = ../beads/sandbox.sh;
      "lib/sandbox/darwin/nix-bridge.c" = ../sandbox/darwin/nix-bridge.c;
      "lib/services/celler-work-drain-state.rs" = ./celler-work-drain-state.rs;
      "crates/wrix-service/src/lifecycle/drain/native" =
        ../../crates/wrix-service/src/lifecycle/drain/native;
    }
    // exampleSources;
    nativeBuildInputs = [ pkgs.git ];

    meta = {
      description = "Rust wrix service and cache CLI foundation";
      mainProgram = "wrix";
    };
  };

  workspace = rustProfile.buildPackage workspaceArgs;
  clientCargoArgs = concatStringsSep " " [
    "--package wrix-cli --package wrix-cache --package wrix-notify"
    "--bin wrix --bin wrix-git-sign --bin wrix-focus-notification"
    "--bin wrix-cache-hook --bin wrix-cache-publish --bin wrix-host-cache-policy"
  ];
  clientCargoArtifacts = workspace.cargoArtifacts.overrideAttrs {
    pname = "wrix-client-deps";
    doCheck = false;
    buildPhase = ''
      runHook preBuild
      cargo --version
      cargoWithProfile build --locked ${clientCargoArgs}
      runHook postBuild
    '';
  };
  clientWorkspace = rustProfile.buildPackage (
    workspaceArgs
    // {
      cargoArtifacts = clientCargoArtifacts;
      cargoExtraArgs = clientCargoArgs;
      srcFilter = import ./rust-client-source-filter.nix {
        inherit (pkgs) lib;
        inherit (rustProfile.craneLib) filterCargoSources;
        root = ../..;
      };
    }
  );
  compileOnlyWorkspace = rustProfile.buildPackage (
    (import ./rust-compile-only-source.nix workspaceArgs)
    // {
      inherit (workspace) cargoArtifacts;
    }
  );
  notificationWorkspace = rustProfile.buildPackage (
    workspaceArgs
    // {
      inherit (workspace) cargoArtifacts;
      cargoExtraArgs = "--package wrix-cli --package wrix-sandbox --lib --test native_notify";
      extraSrcs =
        builtins.removeAttrs workspaceArgs.extraSrcs (
          [
            "tests"
            "loom.toml"
          ]
          ++ map (name: "crates/${name}/tests") (builtins.attrNames (builtins.readDir ../../crates))
          ++ builtins.attrNames exampleSources
        )
        // {
          "tests/prepare-isolated-host-roots.sh" = ../../tests/prepare-isolated-host-roots.sh;
          "tests/prepare-native-fixtures.sh" = ../../tests/prepare-native-fixtures.sh;
          "crates/wrix-cli/tests/native_notify.rs" = ../../crates/wrix-cli/tests/native_notify.rs;
          "crates/wrix-cli/tests/workspace/isolation.rs" = ../../crates/wrix-cli/tests/workspace/isolation.rs;
          "crates/wrix-cli/tests/fixtures/workspace-chroot.sh" =
            ../../crates/wrix-cli/tests/fixtures/workspace-chroot.sh;
          "crates/wrix-sandbox/tests/fixtures/apple-service-image" =
            ../../crates/wrix-sandbox/tests/fixtures/apple-service-image;
        };
    }
  );
  notificationMetadata = import ../util/rust-target-metadata.nix {
    inherit pkgs;
    root = ../..;
    members = builtins.attrNames (builtins.readDir ../../crates);
    includeProduction = false;
  };
  notificationSource = pkgs.runCommand "wrix-notification-source" { } ''
    set -euo pipefail
    cp -r ${notificationMetadata}/. "$out"
    chmod -R u+w "$out"
    cp -r ${notificationWorkspace.nextest.src}/. "$out"
    chmod -R u+w "$out"
  '';
  runtimeWorkspace = rustProfile.buildPackage (
    workspaceArgs
    // {
      inherit (workspace) cargoArtifacts;
      cargoExtraArgs = "--package wrix-cli --package wrix-service-server --bin wrix-service-supervisor --bin wrix-shared-service --bin wrix-egress-gateway --bin wrix-nix-pool-owner";
    }
  );

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
      workspace.bin
      pkgs.nix
      pkgs.skopeo
      gitMinimal
      pkgs.openssl
      pkgs.openssh
    ]
    ++ optionals pkgs.stdenv.hostPlatform.isLinux [
      pkgs.systemd
      pkgs.sshfs
      pkgs.coreutils
      pkgs.fuse3
      (import ../sandbox/linux/krun-runtime.nix { linuxPkgs = pkgs; })
    ]
    ++ optionals pkgs.stdenv.hostPlatform.isDarwin [
      pkgs.coreutils
      pkgs.sshfs-fuse
      (pkgs.writeShellScriptBin "launchctl" ''
        exec /bin/launchctl "$@"
      '')
      (import ./nix-pool/apple-host.nix { inherit pkgs; })
    ];
  };
  poolHostTools = pkgs.writeText "wrix-nix-pool-host-tools.json" (
    builtins.toJSON {
      version = 1;
      root = builtins.appendContext (builtins.unsafeDiscardStringContext (toString controllerRuntime)) (
        builtins.getContext (builtins.unsafeDiscardOutputDependency controllerRuntime.drvPath)
      );
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
          --set WRIX_CONTROLLER_MANAGEMENT "${controllerManagement}" \
          --run 'export WRIX_HOST_RUNTIME_PATH="''${WRIX_HOST_RUNTIME_PATH:-$PATH}"' \
          --prefix PATH : "${controllerRuntime}/bin" ${
            if nixPoolImage == null then
              ""
            else
              ''--set WRIX_NIX_POOL_PACKAGE_BINDING "${poolBinding}" --set WRIX_NIX_POOL_HOST_TOOLS "${poolHostTools}"''
          }
      '';
  controllerManagement = pkgs.symlinkJoin {
    name = "wrix-controller-management";
    paths = [
      pkgs.nix
    ]
    ++ optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.systemd ]
    ++ optionals pkgs.stdenv.hostPlatform.isDarwin [
      (pkgs.writeShellScriptBin "launchctl" ''
        exec /bin/launchctl "$@"
      '')
    ];
  };
  controllerBinding = import ./control-binding.nix {
    inherit pkgs;
    management = controllerManagement;
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
        gitMinimal
        pkgs.dolt
        pkgs.openssl
      ]}"
    ]
    ++ optionals pkgs.stdenv.hostPlatform.isLinux [
      "--set"
      "WRIX_WORKER_SECCOMP"
      "${import ../sandbox/linux/seccomp.nix { inherit pkgs; }}"
    ]
    ++ optionals (nixPoolImage != null) [
      "--set"
      "WRIX_NIX_POOL_PACKAGE_BINDING"
      "${poolBinding}"
    ]
    ++ optionals (nixPoolImage != null) [
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
          (import ../sandbox/linux/krun-runtime.nix { linuxPkgs = pkgs; })
        ]

      )}"
    ]
  );

  cellerPackage = import ./celler.nix {
    inherit pkgs;
    inherit (rustProfile) craneLib;
  };

  fixtureSource = pkgs.runCommand "wrix-fixture-source" { } ''
    cp -r ${workspace.bin.src} "$out"
    chmod -R u+w "$out"
    cp -r ${../../crates/wrix-service/examples} "$out/crates/wrix-service/examples"
  '';

  fixtureExamples = workspace.bin.overrideAttrs (old: {
    pname = "wrix-test-examples";
    src = fixtureSource;
    buildPhase =
      assert pkgs.lib.assertMsg (
        builtins.length (pkgs.lib.splitString "cargoWithProfile build" old.buildPhase) == 2
      ) "Crane native fixture build command changed; review the explicit example target";
      builtins.replaceStrings
        [ "cargoWithProfile build" ]
        [
          "cargoWithProfile build -p wrix-service-server --example host-controller-fixture --example wrix-native-fixtures"
        ]
        old.buildPhase;
    doCheck = true;
    checkPhase = ''
      runHook preCheck
      cargoWithProfile test --offline --locked -p wrix-service-server --example wrix-native-fixtures
      test ! -e ${workspace.bin}/bin/wrix-native-fixtures
      test ! -e ${workspace.bin}/bin/host-controller-fixture
      runHook postCheck
    '';
    installPhase = ''
      mkdir -p "$out/bin"
      cp "target/''${CARGO_BUILD_TARGET:+$CARGO_BUILD_TARGET/}release/examples/wrix-native-fixtures" "$out/bin/"
      cp "target/''${CARGO_BUILD_TARGET:+$CARGO_BUILD_TARGET/}release/examples/host-controller-fixture" "$out/bin/wrix-host-controller"
    '';
    meta = old.meta // {
      mainProgram = "wrix-native-fixtures";
    };
  });

  fixtures = import ../../tests/prek/native-fixtures.nix {
    inherit pkgs;
    fixtureRuntime = fixtureExamples;
    celler = cellerPackage;
    packagedWrix = wrixPackage;
    controller = fixtureExamples;
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
            ''ln -s "${clientWorkspace.bin}/bin/${name}" "$out/bin/${name}"''
          else
            ''makeWrapper "${clientWorkspace.bin}/bin/${name}" "$out/bin/${name}" --prefix PATH : "${makeBinPath runtime}"''
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
        makeWrapper "${clientWorkspace.bin}/bin/${name}" "$out/bin/${name}" \
          --run 'export WRIX_HOST_RUNTIME_PATH="''${WRIX_HOST_RUNTIME_PATH:-$PATH}"' \
          --set WRIX_INVOKED_BIN "$out/bin/${name}" ${concatStringsSep " " wrapperArgs}
        ln -s "${clientWorkspace.bin}/bin/wrix-git-sign" "$out/bin/wrix-git-sign"
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
        ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
          pkgs.util-linux
          pkgs.iproute2
        ];
      preCheck = preCheck + ''
        HOME="$(mktemp -d)"
        export HOME
        git -c init.defaultBranch=main init --quiet
      '';
      WRIX_TEST_PUBLISHER_HELPER = "${cacheHookTestPublisher}/bin/wrix-cache-test-publisher";
    }
    // fixtures.environment
  );

  notificationNextest = notificationWorkspace.nextest.overrideAttrs (old: {
    src = notificationSource;
    nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.tmux ];
  });

  cacheBackendTests = compileOnlyWorkspace.nextest.overrideAttrs (old: {
    # Crane restores dependencies in postPatch. Select the immutable target
    # before that hook, not in buildPhase where it strands the restored cache.
    prePatch = (old.prePatch or "") + ''
      export CARGO_TARGET_DIR="$out/target"
    '';
    pname = "wrix-celler-backend-tests";
    doCheck = false;
    doInstallCargoArtifacts = false;
    buildPhase = ''
      runHook preBuild
      # Runtime fixture reads use CARGO_MANIFEST_DIR. Compile against retained
      # immutable sources, not the disposable Nix build directory. Do not copy
      # unpacked dependency artifacts into this source tree.
      mkdir -p "$out/source"
      tar --exclude='./target' -cf - . | tar -xf - -C "$out/source"
      cd "$out/source"
      : > compiled-tests.json
      for target in celler_backend host_publication_only host_signed_exclusion; do
        cargo test --offline --locked --no-run -p wrix-cache --test "$target" \
          ''${CARGO_PROFILE:+--profile "$CARGO_PROFILE"} --message-format json >> compiled-tests.json
      done
      cargo test --offline --locked --no-run -p wrix-service-server --test celler_auth \
        ''${CARGO_PROFILE:+--profile "$CARGO_PROFILE"} --message-format json >> compiled-tests.json
      for target in service_lifecycle workspace_admission; do
        cargo test --offline --locked --no-run -p wrix-cli --test "$target" \
          ''${CARGO_PROFILE:+--profile "$CARGO_PROFILE"} --message-format json >> compiled-tests.json
      done
      for target in execution_lifetime runtime_environment; do
        cargo test --offline --locked --no-run -p wrix-sandbox --test "$target" \
          ''${CARGO_PROFILE:+--profile "$CARGO_PROFILE"} --message-format json >> compiled-tests.json
      done
      runHook postBuild
    '';
    installPhase = ''
      for target in celler_backend celler_auth host_publication_only host_signed_exclusion service_lifecycle workspace_admission execution_lifetime runtime_environment; do
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
  });

  testFixtures = fixtures.bundle;
  testFixtureEnvironment = fixtures.environment;
  package = workspace.bin;
  clientPackage = clientWorkspace.bin;
  inherit clientCargoArtifacts;
  runtimePackage = runtimeWorkspace.bin;
  wrix = wrixPackage;
  binding = serviceBinding;
  inherit
    controllerRuntime
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
        install -Dm755 "${runtimeWorkspace.bin}/bin/wrix-service-supervisor" "$out/bin/wrix-service-supervisor"
        install -Dm755 "${runtimeWorkspace.bin}/bin/wrix-shared-service" "$out/bin/wrix-shared-service"
        install -Dm755 "${
          import ./gateway.nix {
            inherit pkgs;
            rustPackage = runtimeWorkspace.bin;
          }
        }/bin/wrix-egress-gateway" "$out/bin/wrix-egress-gateway"
      '';
  nixPoolOwner = pkgs.runCommand "wrix-nix-pool-owner" { meta = binaryMeta "wrix-nix-pool-owner"; } ''
    install -Dm755 "${runtimeWorkspace.bin}/bin/wrix-nix-pool-owner" "$out/bin/wrix-nix-pool-owner"
  '';
  hostCachePolicy = mkBinaryPackage "wrix-host-cache-policy" [ ];
  cacheHook = mkBinaryPackage "wrix-cache-hook" [ ];
  cachePublish = mkBinaryPackage "wrix-cache-publish" [
    gitMinimal
    pkgs.nix
  ];
}
