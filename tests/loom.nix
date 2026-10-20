{
  pkgs,
  linuxPkgs,
  wrix,
  loomCli,
  linuxLoomCli,
  packages,
  devShell,
  lock,
}:

let
  inherit (builtins) all attrNames elem;
  inherit (pkgs.lib) concatMapStrings hasPrefix optionalString;
  resources = import ./lib/browser-resources.nix { };
  sourceNode = lock.nodes.${lock.nodes.root.inputs.loom-src};
  sandboxNames = builtins.filter (hasPrefix "sandbox") (attrNames packages) ++ [
    "default"
    "debug"
  ];
  environmentFor = name: packages.${name}.image.profileEnv;
  hostTools = devShell.nativeBuildInputs ++ devShell.buildInputs;
  profileManifest = packages.profile-images;
  nativeEntry = profileManifest.manifest.rust.pi;
  nativeSandbox = profileManifest.sandboxes.rust.pi;
  sharedNixTests = import ./sandbox/shared-nix.nix {
    inherit pkgs linuxPkgs wrix;
    profileSandboxes = profileManifest.sandboxes;
  };

  nativeProfiles =
    assert
      attrNames profileManifest.manifest == [
        "base"
        "python"
        "rust"
      ];
    assert all (profile: attrNames profile == [ "pi" ]) (builtins.attrValues profileManifest.manifest);
    assert nativeEntry.native_preflight == "rust";
    assert nativeSandbox.image.labels."wrix.nix.private" == "1";
    assert all
      (
        name:
        profileManifest.sandboxes.${name}.pi.image.source.drvPath == (wrix.mkSandbox {
          profile = wrix.profiles.${name};
          agent = "pi";
          packages = [ linuxLoomCli ];
        }).image.source.drvPath
      )
      [
        "base"
        "python"
      ];
    assert all (name: profileManifest.sandboxes.${name}.pi.image.labels."wrix.nix.private" == "1") [
      "base"
      "python"
      "rust"
    ];
    assert nativeEntry.runtime == "pi";
    assert nativeEntry.source == toString nativeSandbox.image.source;
    assert nativeEntry.profile_config == toString nativeSandbox.profileConfig;
    assert nativeEntry.launcher == "${nativeSandbox.launcher}/bin/wrix";
    assert all builtins.hasContext [
      nativeEntry.source
      nativeEntry.profile_config
      nativeEntry.launcher
    ];
    pkgs.runCommand "test-loom-native-profiles" { } ''
      set -euo pipefail
      touch "$out"
    '';

  sourceOnly =
    assert sourceNode.flake == false;
    assert !(sourceNode ? inputs);
    assert sourceNode.original.ref == "main";
    assert sourceNode.locked ? rev;
    pkgs.runCommand "test-loom-source-only" { } ''
      set -euo pipefail
      touch "$out"
    '';

  wiring =
    assert loomCli.system == pkgs.stdenv.hostPlatform.system;
    assert linuxLoomCli.stdenv.hostPlatform.isLinux;
    assert toString loomCli.src == toString linuxLoomCli.src;
    assert elem loomCli hostTools;
    assert all (name: elem linuxLoomCli (environmentFor name).paths) sandboxNames;
    pkgs.runCommand "test-loom-package-wiring" { } ''
      set -euo pipefail
      touch "$out"
    '';

  devshellCli =
    assert elem loomCli hostTools;
    pkgs.runCommand "test-loom-devshell" { nativeBuildInputs = [ loomCli ]; } ''
      set -euo pipefail
      [[ "$(command -v loom)" == "${loomCli}/bin/loom" ]]
      [[ ! -e "${loomCli}/bin/loom-direct-runner" ]]
      [[ ! -e "${loomCli}/bin/mock-loom-agent" ]]
      [[ ! -e "${loomCli}/bin/bd-shim" ]]
      loom gate verify --help > "$out"
    '';

  agentImages =
    assert all (name: elem linuxLoomCli (environmentFor name).paths) sandboxNames;
    pkgs.runCommand "test-loom-agent-images" { } (
      ''
        set -euo pipefail
        mkdir -p "$out"
      ''
      + optionalString pkgs.stdenv.hostPlatform.isLinux (
        concatMapStrings (name: ''
          PATH="${environmentFor name}/bin" loom gate verify --help > "$out/${name}"
        '') sandboxNames
      )
    );

  nativeReadiness = pkgs.writeShellApplication {
    name = "test-loom-native-readiness";
    runtimeInputs = [
      pkgs.podman
      pkgs.coreutils
    ];
    text = ''
      image=$(podman pull --quiet --signature-policy ${pkgs.writeText "local-image-policy.json" ''
        {"default":[{"type":"insecureAcceptAnything"}]}
      ''} oci:${nativeSandbox.image.ociLayout}:latest)
      expected=$(cat ${nativeSandbox.image.digest})
      actual=$(podman image inspect --format '{{.Id}}' "$image")
      [[ "''${actual#sha256:}" == "''${expected#sha256:}" ]]
      podman run --rm --network=pasta --user=1000:1000 --workdir=/tmp \
        --mount type=bind,source=${pkgs.path},destination=/fixture/nixpkgs,ro \
        --mount type=bind,source=${./sandbox/native-nix-operations.sh},destination=/fixture/probe.sh,ro \
        --entrypoint /bin/bash "$expected" /fixture/probe.sh /fixture/nixpkgs
    '';
  };

  ciApp = name: check: {
    inherit name;
    inherit (resources) inputs;
    executable = name;
    package = pkgs.writeShellScriptBin name ''
      set -euo pipefail
      [[ -e "${check}" ]]
    '';
  };
in
{
  checks = {
    loom-source-only = sourceOnly;
    loom-package-wiring = wiring;
    loom-devshell = devshellCli;
    loom-native-profiles = nativeProfiles;
  }
  // sharedNixTests.checks
  // import ./sandbox/private-nix.nix {
    inherit
      pkgs
      linuxPkgs
      wrix
      ;
    profileSandboxes = profileManifest.sandboxes;
  };
  ciApps = sharedNixTests.ciApps ++ [
    (ciApp "test-loom-devshell" devshellCli)
    (ciApp "test-loom-agent-images" agentImages)
    {
      name = "test-loom-native-readiness";
      inherit (resources) inputs;
      executable = "test-loom-native-readiness";
      package = nativeReadiness;
      platforms = (import ./lib/verifier.nix).linux;
      capabilities = [ "container-runtime" ];
    }
  ];
}
