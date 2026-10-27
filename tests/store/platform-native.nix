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
        WRIX_POOL_TEST = "installed";
        args = [ "-euc" (
          "set -euo pipefail; [[ $(" + core + "/bin/id -u) != 0 ]]; " +
          "[[ ! -e /var/empty/native-platform-outside-build && ! -e /var/lib/wrix/nix-pool/host-key && ! -e /run/wrix-nix-pool && ! -e /workspace && ! -e /control ]]; " +
          core + "/bin/mkdir -p $out/bin; " + core + "/bin/id -u > $out/build-uid; " +
          core + "/bin/printf '#!" + bash + "/bin/bash\\necho installed\\n' > $out/bin/probe; " +
          core + "/bin/chmod +x $out/bin/probe"
        ) ];
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
  app =
    platform: test: fault:
    pkgs.writeShellScriptBin "test-shared-nix-${platform}" ''
      set -euo pipefail
      ${optionalString (!isDarwin) ''
        export WRIX_TEST_POOL_CLEANUP_TOOLS
        WRIX_TEST_POOL_CLEANUP_TOOLS=$(${pkgs.jq}/bin/jq -er '.root + "/bin"' ${wrix.rustPackage.poolHostTools})
        export WRIX_TEST_POOL_CLEANUP_SCRIPT=${./cleanup-native-fixtures.py}
        export WRIX_TEST_POOL_PYTHON=${pkgs.python3}/bin/python3
      ''}
      export WRIX_NIX_POOL_PACKAGE_BINDING=${wrix.rustPackage.poolBinding}
      export WRIX_NIX_POOL_HOST_TOOLS=${wrix.rustPackage.poolHostTools}
      export WRIX_CONTROLLER_PACKAGE_BINDING=${wrix.rustPackage.controllerBinding}
      export WRIX_TEST_POOL_CONTROLLER_BINDING=${wrix.rustPackage.controllerBinding}
      export WRIX_TEST_POOL_CONTROLLER=${wrix.rustPackage.controllerPackage}/bin/wrix-host-controller
      export WRIX_TEST_POOL_PROFILE=${worker.profileConfig}
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
