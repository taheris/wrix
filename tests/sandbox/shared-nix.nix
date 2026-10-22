{
  pkgs,
  linuxPkgs,
  wrix,
  profileSandboxes,
}:
let
  inherit (builtins)
    all
    attrNames
    hasContext
    toJSON
    toString
    ;
  resources = import ../lib/browser-resources.nix { };
  configOnly = linuxPkgs.writeText "configuration-only-extension.js" ''
    export default function () {}
  '';
  mkFixture =
    agent: settings:
    wrix.mkSandbox {
      inherit agent;
      agentPkg = linuxPkgs.hello;
      agentSettings = settings;
    };
  fixtures =
    map
      (
        agent:
        mkFixture agent {
          env.WRIX_CONFIGURATION_REFERENCE = "${configOnly}";
        }
      )
      [
        "claude"
        "pi"
      ];
  plain = mkFixture "pi" { };
  changed = mkFixture "pi" { env.WRIX_CONFIGURATION_REFERENCE = "${configOnly}"; };
  plaintextChanged = mkFixture "pi" {
    env.WRIX_CONFIGURATION_REFERENCE = "${configOnly}";
    env.WRIX_PLAINTEXT_VARIANT = "does-not-change-the-installed-store";
  };
  configurationRoots = import ../../lib/sandbox/configuration-roots.nix { inherit (pkgs) lib; };
  archive = import ../../lib/sandbox/image.nix {
    pkgs = linuxPkgs;
    hostPkgs = pkgs;
    asTarball = true;
    profile = wrix.profiles.base;
    entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
    agent = "direct";
    agentPkg = linuxPkgs.hello;
    claudeConfig = { };
    claudeSettings = { };
    mcpServerConfigs.fixture = {
      command = "${linuxPkgs.coreutils}/bin/cat";
      env.WRIX_CONFIGURATION_REFERENCE = "${configOnly}";
    };
  };
  inspect =
    image:
    pkgs.runCommandLocal "shared-nix-image-inspection" { nativeBuildInputs = [ pkgs.python3 ]; } ''
      set -euo pipefail
      python ${../lib/image-artifact.py} prepare ${image.source} "$out"
    '';
  imageCase = image: {
    inherit (image) source_kind;
    source = toString image.source;
    artifact = toString (inspect image);
    shared = image.sharedNix;
  };
  sandboxCase =
    sandbox:
    (imageCase sandbox.image)
    // {
      profile_config = toString sandbox.image.profileConfig;
      digest_file = toString sandbox.image.digest;
    };
  cases =
    (map (name: sandboxCase profileSandboxes.${name}.pi) (attrNames profileSandboxes))
    ++ (map (
      sandbox: (sandboxCase sandbox) // { required_paths = [ (toString configOnly) ]; }
    ) fixtures)
    ++ [ ((imageCase archive) // { required_paths = [ (toString configOnly) ]; }) ];
  casesFile = pkgs.writeText "shared-nix-images.json" (toJSON cases);
  api =
    assert
      configurationRoots {
        nested = [
          configOnly
          "${configOnly}/bin/tool"
        ];
      } == [
        (toString configOnly)
      ];
    assert configurationRoots "${configOnly}?annotation" == [ (toString configOnly) ];
    assert configurationRoots configOnly.drvPath == [ configOnly.drvPath ];
    assert configurationRoots (pkgs.lib.strings.addContextFrom configOnly "plaintext") == [ ];
    assert configurationRoots (builtins.unsafeDiscardStringContext "${configOnly}") == [ ];
    assert
      configurationRoots [
        null
        true
        1
        { ignored = "plaintext"; }
      ] == [ ];
    assert wrix.capabilities.privateNix == 1;
    assert wrix.capabilities.sharedNix == 1;
    assert all (
      sandbox:
      sandbox.image.labels."wrix.nix.private" == "1"
      && sandbox.image.labels."wrix.nix.shared" == "1"
      && sandbox.image.sharedNix.version == wrix.capabilities.sharedNix
      && hasContext sandbox.image.sharedNix.seed_closure
    ) ([ plain ] ++ fixtures);
    assert archive.labels."wrix.nix.shared" == "1";
    assert archive.sharedNix.version == 1;
    assert archive.source_kind == "docker-archive";
    assert plain.image.baseImage.drvPath == changed.image.baseImage.drvPath;
    assert plain.image.stableProfileImage.drvPath == changed.image.stableProfileImage.drvPath;
    assert plain.image.agentImage.drvPath == changed.image.agentImage.drvPath;
    assert plain.image.sharedNix.seed_closure != changed.image.sharedNix.seed_closure;
    assert changed.image.sharedNix.seed_closure == plaintextChanged.image.sharedNix.seed_closure;
    assert changed.image.ociLayout.storeLayers == plaintextChanged.image.ociLayout.storeLayers;
    assert changed.image.source != plaintextChanged.image.source;
    pkgs.runCommandLocal "shared-nix-api" { } ''touch "$out"'';
  artifacts =
    pkgs.runCommandLocal "shared-nix-image-contract" { nativeBuildInputs = [ pkgs.python3 ]; }
      ''
        set -euo pipefail
        [[ -e ${api} ]]
        python ${./shared-nix-image.py} ${casesFile} ${../lib/image-artifact.py} ${./native-nix-permissions.py}
        touch "$out"
      '';
in
{
  checks.shared-nix-api = api;
  ciApps = [
    {
      name = "test-shared-nix-image-contract";
      executable = "test-shared-nix-image-contract";
      inherit (resources) inputs;
      package = pkgs.writeShellScriptBin "test-shared-nix-image-contract" ''
        set -euo pipefail
        [[ -e ${artifacts} ]]
      '';
    }
  ];
}
