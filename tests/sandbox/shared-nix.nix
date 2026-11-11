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
  resources = import ../lib/image-resources.nix { };
  verifyTools = import ../../lib/verify/tools.nix { inherit pkgs; };
  coldLib = builtins.path {
    path = pkgs.path + "/lib";
    name = "configuration-nixpkgs-lib";
  };
  configOnly = linuxPkgs.writeText "configuration-only-extension.js" ''
    export default function () {}
  '';
  questionName = linuxPkgs.writeText "configuration?native-name.js" "native";
  multiOutput =
    pkgs.runCommand "configuration-multiple-outputs"
      {
        outputs = [
          "out"
          "dev"
        ];
      }
      ''
        mkdir "$out" "$dev"
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
  configurationRoots = import (resources.sandbox.path + "/configuration-roots.nix") { inherit pkgs; };
  referenceCases = [
    {
      input = questionName;
      expected = [ (toString questionName) ];
    }
    {
      input = "${multiOutput.dev}/include";
      expected = [ (toString multiOutput.dev) ];
    }
    {
      input.nested = [
        configOnly
        "${configOnly}/bin/tool"
      ];
      expected = [ (toString configOnly) ];
    }
    {
      input = "${configOnly}?annotation";
      expected = [ (toString configOnly) ];
    }
    {
      input = configOnly.drvPath;
      expected = [ configOnly.drvPath ];
    }
    {
      input = pkgs.lib.strings.addContextFrom configOnly "plaintext";
      expected = [ ];
    }
    {
      input = builtins.unsafeDiscardStringContext "${configOnly}";
      expected = [ ];
    }
    {
      input = [
        null
        true
        1
        { ignored = "plaintext"; }
      ];
      expected = [ ];
    }
  ];
  referenceResults = pkgs.writeText "configuration-reference-cases.json" (
    toJSON (
      map (case: {
        root = configurationRoots case.input;
        expected = map builtins.unsafeDiscardOutputDependency case.expected;
      }) referenceCases
    )
  );
  configurationRootCheck = import ./configuration-roots-check.nix { inherit pkgs; };
  archive = import (resources.sandbox.path + "/image.nix") {
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
    pkgs.runCommandLocal "shared-nix-image-inspection" { } ''
      set -euo pipefail
      ${pkgs.lib.getExe verifyTools} image-artifact prepare ${image.source} "$out"
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
    pkgs.runCommandLocal "shared-nix-api" { } ''
      # The legacy evaluator defaults to read-only. The enclosing sandbox
      # supplies these immutable source files; no live daemon/store is used.
      ${pkgs.lib.getBin pkgs.nix}/bin/nix-instantiate --eval --strict --json --store dummy:// --option allow-import-from-derivation false --expr '
        let lib = import ${coldLib}; in
        import ${./configuration-cold.nix} {
          inherit lib;
          candidates = import ${../../lib/sandbox/configuration-candidates.nix} { inherit lib; };
          system = "${pkgs.stdenv.hostPlatform.system}";
        }
      ' > cold.json
      ${pkgs.jq}/bin/jq -se 'length == 1 and .[0] == true' cold.json >/dev/null
      test -e ${import ./image-tools.nix { inherit pkgs; }}
      checker=${configurationRootCheck}/bin/check-configuration-roots
      "$checker" ${referenceResults}
      ${pkgs.jq}/bin/jq 'map(if .root != null then .expected = [] else . end)' ${referenceResults} > wrong-targets.json
      if "$checker" wrong-targets.json; then
        echo 'configuration root checker accepted corrupted expectations' >&2
        exit 1
      fi
      mkdir -p "$TMPDIR/not-a-link/share/wrix/image-inputs"
      touch "$TMPDIR/not-a-link/share/wrix/image-inputs/regular-file"
      ${pkgs.jq}/bin/jq -n --arg root "$TMPDIR/not-a-link" '[{root:$root,expected:[]}]' > nonlink.json
      if "$checker" nonlink.json; then
        echo 'configuration root checker accepted a non-symlink entry' >&2
        exit 1
      fi
      printf '%s\n' '[{"root":null,"expected":["unexpected"]}]' > inconsistent.json
      if "$checker" inconsistent.json; then
        echo 'configuration root checker accepted inconsistent null roots' >&2
        exit 1
      fi
      printf '{' > malformed.json
      if "$checker" malformed.json; then
        echo 'configuration root checker accepted malformed JSON' >&2
        exit 1
      fi
      printf '%s\n' '[] []' > multiple.json
      if "$checker" multiple.json; then
        echo 'configuration root checker accepted multiple JSON documents' >&2
        exit 1
      fi
      touch "$out"
    '';
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
