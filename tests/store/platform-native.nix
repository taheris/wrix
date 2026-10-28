{ pkgs, wrix }:
let
  inherit (pkgs.stdenv.hostPlatform) isDarwin;
  inherit (pkgs.lib) makeBinPath optionalAttrs optionalString;
  guest =
    if isDarwin then
      import pkgs.path {
        system = "aarch64-linux";
        config.allowUnfree = true;
        inherit (pkgs) overlays;
      }
    else
      pkgs;
  coldPath = builtins.unsafeDiscardStringContext (toString guest.hello);
  unsigned = pkgs.writeText "wrix-native-unsigned-input" "unsigned native platform control\n";
  workflow = guest.writeText "wrix-native-workflow.nix" ''
    let
      bash = builtins.storePath "${guest.bash}";
      core = builtins.storePath "${guest.coreutils}";
      drv = builtins.derivation {
        name = "wrix-native-installed-platform";
        system = "${guest.stdenv.hostPlatform.system}";
        builder = bash + "/bin/bash";
        stdenv = builtins.storePath "${guest.stdenvNoCC}";
        WRIX_POOL_TEST = "installed";
        args = [ "-e" (builtins.storePath "${guest.writeText "native-default-builder.sh" ''
          source "$stdenv/setup"
          genericBuild
        ''}") ];
        buildCommand = (
          "set -euo pipefail; [[ $(" + core + "/bin/id -u) != 0 ]]; " +
          "[[ ! -e /var/empty/native-platform-outside-build && ! -e /var/lib/wrix/nix-pool/host-key && ! -e /run/wrix-nix-pool && ! -e /workspace && ! -e /control ]]; " +
          core + "/bin/mkdir -p $out/bin; " + core + "/bin/id -u > $out/build-uid; " +
          core + "/bin/printf '#!" + bash + "/bin/bash\\necho installed\\n' > $out/bin/probe; " +
          core + "/bin/chmod +x $out/bin/probe"
        );
      };
    in drv // { meta.mainProgram = "probe"; }
  '';
  peerWorkflow = guest.writeText "native-peer-workflow.nix" ''
    builtins.derivation {
      name = "wrix-native-peer-only";
      system = "${guest.stdenv.hostPlatform.system}";
      builder = (builtins.storePath "${guest.bash}") + "/bin/bash";
      args = [ "-c" "set -euo pipefail; printf peer-only > $out" ];
    }
  '';
  bashFlake = guest.writeTextDir "flake.nix" ''
    { outputs = { ... }: { legacyPackages.${guest.stdenv.hostPlatform.system}.bashInteractive = builtins.storePath "${guest.bashInteractive}"; }; }
  '';
  resources = guest.writeText "native-resources.nix" ''
    kind: import ${./native-resources.nix} {
      bash = builtins.storePath "${guest.bash}";
      core = builtins.storePath "${guest.coreutils}";
      util = builtins.storePath "${guest.util-linux}";
      python = builtins.storePath "${guest.python3}";
      curl = builtins.storePath "${guest.curl}";
      system = "${guest.stdenv.hostPlatform.system}";
    } kind
  '';
  nativeInputs = pkgs.writeText "native-service-observations.json" (
    builtins.toJSON {
      python = "${guest.python3}/bin/python3";
      probe = toString ./native-boundary.py;
    }
  );
  checks = guest.runCommand "native-store-flake-checks" { } ''
    set -euo pipefail
    mkdir -p "$out"
    cp ${guest.writeText "flake.nix" ''
      { outputs = { ... }: {
        checks.${guest.stdenv.hostPlatform.system}.installed = import ${workflow};
      }; }
    ''} "$out/flake.nix"
    cp ${guest.writeText "flake.lock" ''
      {"nodes":{"root":{}},"root":"root","version":7}
    ''} "$out/flake.lock"
  '';
  rustProject = guest.runCommand "native-store-rust-project" { } ''
    set -euo pipefail
    mkdir -p "$out/src"
    cp ${guest.writeText "Cargo.toml" ''
      [package]
      name = "native-installed-workflow"
      version = "0.1.0"
      edition = "2024"
    ''} "$out/Cargo.toml"
    cp ${guest.writeText "Cargo.lock" ''
      version = 4
      [[package]]
      name = "native-installed-workflow"
      version = "0.1.0"
    ''} "$out/Cargo.lock"
    cp ${guest.writeText "main.rs" ''
      fn main() { println!("native-installed"); }
      #[test]
      fn installed_workflow() { assert_eq!(2 + 2, 4); }
    ''} "$out/src/main.rs"
  '';
  agent = guest.writeShellScriptBin "wrix-native-platform-worker" ''
    set -euo pipefail
    export PATH=${
      guest.lib.makeBinPath [
        guest.bash
        guest.coreutils
        guest.nix
        guest.jq
        guest.openssh
        guest.python3
        guest.util-linux
        guest.curl
        guest.gnugrep
      ]
    }:"$PATH"
    export WRIX_NATIVE_BOUNDARY=${./native-boundary.py}
    export WRIX_NATIVE_RESOURCES=${resources}
    export WRIX_NATIVE_COLD_PATH=${coldPath}
    export WRIX_NATIVE_CHECKS=${checks}
    export WRIX_NATIVE_PEER_WORKFLOW=${peerWorkflow}
    export WRIX_NATIVE_WORKFLOW=${workflow}
    export WRIX_NATIVE_BASH_FLAKE=${bashFlake}
    export WRIX_NATIVE_PROJECT=${rustProject}
    export WRIX_NATIVE_SYSTEM=${guest.stdenv.hostPlatform.system}
    exec ${guest.bash}/bin/bash ${./native-agent.sh} "$@"
  '';
  worker = wrix.mkSandbox {
    agent = "direct";
    agentPkg = agent;
    profile = wrix.profiles.rust;
  };
  nextWorker = wrix.mkSandbox {
    agent = "direct";
    agentPkg = agent;
    profile = wrix.profiles.rust // {
      env = wrix.profiles.rust.env // {
        WRIX_NATIVE_SEED_VARIANT = "replacement";
      };
    };
  };
  imageCase = { image, ... }: {
    source = toString image.source;
    inherit (image) source_kind;
    digest_file = toString image.digest;
    shared = image.sharedNix;
  };
  images = pkgs.writeText "native-platform-images.json" (
    builtins.toJSON {
      primary = imageCase worker;
      next = imageCase nextWorker;
    }
  );
  observerOwner = import ./native-observer-owner.nix {
    pkgs = guest;
    owner = wrix.nixPoolOwner;
  };
  observerCheck = import ./native-observer-owner-check.nix { pkgs = guest; };
  networkCheck = import ./network-policy-check.nix { pkgs = guest; };
  observerPool = import ../../lib/services/nix-pool/default.nix {
    pkgs = guest;
    owner = observerOwner;
  };
  observerImage = import ../../lib/services/image.nix {
    pkgs = guest;
    hostPkgs = pkgs;
    pool = observerPool;
    asTarball = isDarwin;
  };
  observerBinding = import ../../lib/services/binding.nix {
    inherit pkgs;
    image = observerImage;
  };
  observerController = pkgs.writeShellScriptBin "wrix-host-controller" ''
    set -euo pipefail
    export WRIX_NIX_POOL_PACKAGE_BINDING=${observerBinding}
    export WRIX_NIX_POOL_HOST_TOOLS=${wrix.rustPackage.poolHostTools}
    export PATH=${wrix.rustPackage.controllerRuntime}/bin:"$PATH"
    exec ${wrix.rustPackage.controllerRuntime}/bin/wrix-host-controller "$@"
  '';
  observerControllerBinding = import ../../lib/services/control-binding.nix {
    inherit pkgs;
    controller = observerController;
    runtime = wrix.rustPackage.controllerRuntime;
    image = wrix.serviceImage;
  };
  publicRun = pkgs.replaceVars ./public-run.py { cli = "${wrix.rustPackage.wrix}/bin/wrix"; };
  publicManager =
    pkgs.runCommand "wrix-native-public-manager" { nativeBuildInputs = [ pkgs.stdenv.cc ]; }
      ''
        mkdir -p "$out/bin"
        cc -Wall -Wextra -Werror -O2 -DWRIX_SYSTEMD='"${pkgs.systemd}/lib/systemd/systemd"' ${./public-manager.c} -o "$out/bin/wrix-native-public-manager"
      '';
  app =
    platform: test: fault:
    let
      public = pkgs.lib.hasPrefix "public::" test;
      poolBinding = if public then wrix.rustPackage.poolBinding else observerBinding;
      controllerBinding =
        if public then wrix.rustPackage.controllerBinding else observerControllerBinding;
      controller = if public then wrix.rustPackage.controllerPackage else observerController;
    in
    pkgs.writeShellScriptBin "test-shared-nix-${platform}" ''
      set -euo pipefail
      test -f ${observerCheck}
      test -f ${networkCheck}
      ${optionalString (!isDarwin) ''
        export WRIX_TEST_POOL_CLEANUP_TOOLS
        WRIX_TEST_POOL_CLEANUP_TOOLS=$(${pkgs.jq}/bin/jq -er '.root + "/bin"' ${wrix.rustPackage.poolHostTools})
        export WRIX_TEST_POOL_CLEANUP_SCRIPT=${./cleanup-native-fixtures.py}
        export WRIX_TEST_POOL_PYTHON=${pkgs.python3}/bin/python3
      ''}
      export WRIX_NIX_POOL_PACKAGE_BINDING=${poolBinding}
      export WRIX_NIX_POOL_HOST_TOOLS=${wrix.rustPackage.poolHostTools}
      export WRIX_CONTROLLER_PACKAGE_BINDING=${controllerBinding}
      export WRIX_TEST_POOL_CONTROLLER_BINDING=${controllerBinding}
      export WRIX_TEST_POOL_CONTROLLER=${controller}/bin/wrix-host-controller
      export WRIX_TEST_POOL_PROFILE=${worker.image.profileConfig}
      export WRIX_TEST_POOL_NEXT_PROFILE=${nextWorker.image.profileConfig}
      export WRIX_TEST_POOL_PUBLIC_CLI=${wrix.rustPackage.wrix}/bin/wrix
      export WRIX_TEST_POOL_PUBLIC_RUN=${publicRun}
      ${optionalString (!isDarwin) ''
        export WRIX_TEST_POOL_SYSTEMD=${pkgs.systemd}/lib/systemd/systemd
        export WRIX_TEST_POOL_MANAGER=${publicManager}/bin/wrix-native-public-manager
        export WRIX_TEST_POOL_SYSTEMCTL=${pkgs.systemd}/bin/systemctl
        export WRIX_TEST_POOL_BUSCTL=${pkgs.systemd}/bin/busctl
        export WRIX_TEST_POOL_DBUS=${pkgs.dbus}/bin/dbus-daemon
      ''}
      export WRIX_TEST_POOL_WORKER_IMAGE=${images}
      export WRIX_TEST_POOL_NATIVE_INPUTS=${nativeInputs}
      export WRIX_TEST_POOL_UNSIGNED=${unsigned}
      export WRIX_TEST_POOL_NIX=${pkgs.nix}/bin/nix
      export WRIX_TEST_POOL_NIX_STORE=${pkgs.nix}/bin/nix-store
      export PATH=${
        makeBinPath [
          pkgs.git
          pkgs.bash
          pkgs.coreutils
          pkgs.nix
          pkgs.python3
        ]
      }:"$PATH"
      ${optionalString (!isDarwin) "${pkgs.bash}/bin/bash ${./run-native-test.sh}"} \
        ${wrix.rustPackage.cacheBackendTests}/bin/workspace-admission-tests \
        --ignored --exact "nix_store::admission::request::native::platform::${test}" --nocapture
      ${optionalString (fault != null) ''
        exec ${optionalString (!isDarwin) "${pkgs.bash}/bin/bash ${./run-native-test.sh}"} \
          ${wrix.rustPackage.cacheBackendTests}/bin/workspace-admission-tests \
          --ignored --exact "nix_store::admission::request::native::platform::fault::${fault}" --nocapture
      ''}
    '';
in
optionalAttrs isDarwin {
  apple =
    app "apple" "apple_native_workers_reuse_installed_store_across_independent_vms"
      "apple_native_daemon_death_retains_roots_and_refuses_replay";
}
// optionalAttrs (!isDarwin) {
  krun =
    app "krun" "krun_native_workers_reuse_installed_store_across_independent_vms"
      "krun_native_daemon_death_retains_roots_and_refuses_replay";
  podman = app "podman" "podman_native_workers_reuse_installed_store" null;
}
// optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
  public-podman =
    app "public-podman" "public::podman_public_run_and_spawn_reuse_installed_store"
      null;
  public-krun = app "public-krun" "public::krun_public_run_and_spawn_reuse_installed_store" null;
}
