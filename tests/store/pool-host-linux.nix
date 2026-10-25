{ pkgs, wrix }:

let
  unsigned = pkgs.writeText "wrix-worker-unsigned-input" "unsigned worker import control\n";
  workflow = pkgs.writeText "wrix-installed-worker.nix" ''
    let
      bash = builtins.storePath "${pkgs.bash}";
      core = builtins.storePath "${pkgs.coreutils}";
      drv = builtins.derivation {
        name = "wrix-installed-worker";
        outputs = [ "out" ];
        system = "${pkgs.stdenv.hostPlatform.system}";
        builder = bash + "/bin/bash";
        WRIX_POOL_TEST = "installed";
        args = [ "-c" (core + "/bin/mkdir -p $out/bin; " + core + "/bin/printf '#!" + bash + "/bin/bash\\necho installed\\n' > $out/bin/probe; " + core + "/bin/chmod +x $out/bin/probe") ];
      };
    in drv // { meta.mainProgram = "probe"; }
  '';
  escapeWorkflow = pkgs.writeText "wrix-worker-escape.nix" ''
    let
      bash = builtins.storePath "${pkgs.bash}";
      core = builtins.storePath "${pkgs.coreutils}";
    in builtins.derivation {
      name = "wrix-worker-escape-control";
      system = "${pkgs.stdenv.hostPlatform.system}";
      builder = bash + "/bin/bash";
      args = [ "-c" (core + "/bin/ln -s " + builtins.readFile /workspace/escape-target + " $out") ];
    }
  '';
  bashFlake = pkgs.writeTextDir "flake.nix" ''
    {
      outputs = { ... }: {
        legacyPackages.${pkgs.stdenv.hostPlatform.system}.bashInteractive = builtins.storePath "${pkgs.bashInteractive}";
      };
    }
  '';
  workerAgent = pkgs.writeShellScriptBin "wrix-pool-worker" ''
    set -euo pipefail
    case "$1" in
      factory-probe)
        [[ "$("${pkgs.nix}/bin/nix" eval --expr '17')" == 17 ]]
        exit 0
        ;;
      factory-lifetime)
        [[ "$("${pkgs.nix}/bin/nix" eval --expr '17')" == 17 ]]
        printf 'ready\n' > /workspace/factory-lifetime-ready
        while [[ ! -f /workspace/factory-lifetime-check ]]; do ${pkgs.coreutils}/bin/sleep 0.1; done
        [[ "$("${pkgs.nix}/bin/nix" eval --expr '18')" == 18 ]]
        "${pkgs.nix}/bin/nix" store info --json | "${pkgs.jq}/bin/jq" -e '.trusted == false' > /dev/null
        printf 'resumed\n' > /workspace/factory-lifetime-resumed
        while [[ ! -f /workspace/factory-lifetime-exit ]]; do ${pkgs.coreutils}/bin/sleep 0.1; done
        exit 0
        ;;
      first|second|replacement) name="$1" ;;
      *) exit 64 ;;
    esac
    path=$("${pkgs.nix}/bin/nix" eval --impure --raw --file ${workflow} --apply 'drv: drv.outPath')
    if [[ "$name" == replacement ]]; then [[ -e "$path" ]]; else [[ ! -e "$path" ]]; fi
    printf 'ready\n' > "/workspace/$name-ready"
    while [[ ! -f /workspace/workers-go ]]; do ${pkgs.coreutils}/bin/sleep 0.1; done
    ${pkgs.nix}/bin/nix build --impure --file ${workflow} --out-link "/workspace/$name-result"
    [[ -e "$path" ]]
    ${pkgs.nix}/bin/nix run --impure --file ${workflow} > "/workspace/$name-run"
    ${pkgs.nix}/bin/nix develop --impure --override-flake nixpkgs path:${bashFlake} --file ${workflow} --command ${pkgs.bash}/bin/bash -c '[[ "$WRIX_POOL_TEST" == installed ]]'
    ${pkgs.coreutils}/bin/mkdir -p "$HOME/.local/state/nix/profiles"
    ${pkgs.nix}/bin/nix profile install --impure --file ${workflow} --profile "$HOME/.local/state/nix/profiles/test"
    [[ "$("$HOME/.local/state/nix/profiles/test/bin/probe")" == installed ]]
    if ${pkgs.coreutils}/bin/touch /nix/store/wrix-worker-mutation; then exit 65; fi
    ${pkgs.nix}/bin/nix build --impure --file ${escapeWorkflow} --out-link "/workspace/$name-escape"
    if ${pkgs.coreutils}/bin/cat "/workspace/$name-escape"; then exit 66; fi
    if ${pkgs.nix}/bin/nix-store --verify --check-contents --repair > "/workspace/$name-repair" 2>&1; then exit 67; fi
    ${pkgs.gnugrep}/bin/grep -E 'not privileged|not allowed|not trusted' "/workspace/$name-repair"
    if ${pkgs.nix}/bin/nix-store --import < /workspace/unsigned.nar > "/workspace/$name-import" 2>&1; then exit 68; fi
    ${pkgs.gnugrep}/bin/grep 'lacks a signature' "/workspace/$name-import"
    if ${pkgs.openssh}/bin/ssh -F /run/wrix/nix-pool/ssh_config root@wrix-nix-pool control < /dev/null > "/workspace/$name-admin" 2>&1; then exit 69; fi
    ${pkgs.gnugrep}/bin/grep 'Permission denied' "/workspace/$name-admin"
    printf 'done\n' > "/workspace/$name-done"
    while [[ ! -f "/workspace/$name-exit" ]]; do ${pkgs.coreutils}/bin/sleep 0.1; done
  '';
  worker = wrix.mkSandbox {
    agent = "direct";
    agentPkg = workerAgent;
  };
  nextWorker = wrix.mkSandbox {
    agent = "direct";
    agentPkg = workerAgent;
    profile = wrix.profiles.base // {
      env = (wrix.profiles.base.env or { }) // {
        WRIX_SEED_VARIANT = "next";
      };
    };
  };
  imageCase = { image, ... }: {
    source = toString image.source;
    inherit (image) source_kind;
    digest_file = toString image.digest;
    shared = image.sharedNix;
  };
  seedImage =
    assert worker.image.sharedNix == nextWorker.image.sharedNix;
    pkgs.writeText "wrix-pool-worker-seed.json" (
      builtins.toJSON {
        primary = imageCase worker;
        next = imageCase nextWorker;
      }
    );
in
assert pkgs.stdenv.hostPlatform.isLinux;
pkgs.writeShellApplication {
  name = "test-shared-nix-host-owner-linux";
  runtimeInputs = [
    pkgs.coreutils
    pkgs.git
    pkgs.nix
    pkgs.openssh
    pkgs.podman
    pkgs.python3
    pkgs.util-linux
  ];
  text = ''
    [[ -r /dev/kvm && -w /dev/kvm ]]
    test -f ${wrix.nixPoolImage.source}
    test -f ${wrix.nixPoolImage.digest}
    export WRIX_TEST_POOL_CLEANUP_TOOLS
    WRIX_TEST_POOL_CLEANUP_TOOLS=$(${pkgs.jq}/bin/jq -er '.root + "/bin"' ${wrix.rustPackage.poolHostTools})
    export WRIX_TEST_POOL_CLEANUP_SCRIPT=${./cleanup-native-fixtures.py}
    export WRIX_TEST_POOL_PYTHON=${pkgs.python3}/bin/python3
    export WRIX_NIX_POOL_PACKAGE_BINDING=${wrix.rustPackage.poolBinding}
    export WRIX_NIX_POOL_HOST_TOOLS=${wrix.rustPackage.poolHostTools}
    export WRIX_TEST_POOL_UNSIGNED=${unsigned}
    export WRIX_TEST_POOL_NIX=${pkgs.nix}/bin/nix
    export WRIX_TEST_POOL_BASH=${pkgs.bashInteractive}
    export WRIX_TEST_POOL_WORKER_IMAGE=${seedImage}
    export WRIX_TEST_POOL_CONTROLLER=${wrix.rustPackage.controllerPackage}/bin/wrix-host-controller
    ${pkgs.bash}/bin/bash ${./run-native-test.sh} \
      ${wrix.rustPackage.cacheBackendTests}/bin/workspace-admission-tests \
      --ignored --exact \
      nix_store::admission::native_host_owner_survives_controller_loss_collects_and_resumes_recorded_software \
      --nocapture
    ${pkgs.bash}/bin/bash ${./run-native-test.sh} \
      ${wrix.rustPackage.cacheBackendTests}/bin/workspace-admission-tests \
      --ignored --exact \
      nix_store::admission::request::native::supervised_controller_prepares_after_requester_exit_and_preserves_native_owner \
      --nocapture
  '';
}
