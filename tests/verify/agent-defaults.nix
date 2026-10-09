{ flake, system }:

let
  inherit (builtins)
    all
    attrNames
    hasAttr
    hasContext
    toJSON
    toString
    tryEval
    ;
  pkgs = flake.inputs.nixpkgs.legacyPackages.${system};
  wlib = flake.legacyPackages.${system}.lib;
  packages = flake.packages.${system};
  imageSystem = if system == "aarch64-darwin" then "aarch64-linux" else system;
  imagePkgs = import flake.inputs.nixpkgs {
    system = imageSystem;
    config.allowUnfree = true;
  };
  runner = imagePkgs.hello;
  piPackage = import ../../lib/sandbox/pi.nix { pkgs = imagePkgs; };
  profiles = [
    "base"
    "python"
    "rust"
  ];
  sandboxName = profile: "sandbox${if profile == "base" then "" else "-${profile}"}";
  sandbox = args: wlib.mkSandbox ({ profile = wlib.profiles.base; } // args);
  selects =
    expected: package: image:
    image.agent == expected
    && image.labels."wrix.agent.kind" == expected
    &&
      map toString (
        pkgs.lib.subtractLists image.stableProfileImage.lowerTiersContents image.agentImage.lowerTiersContents
      ) == [ (toString package) ];
  rejects = args: !(tryEval (sandbox args)).success;
  direct = sandbox {
    agent = "direct";
    agentPkg = runner;
  };
  invalidPrograms = [
    null
    ""
    "."
    ".."
    "/bin/hello"
    "bin/hello"
    "hello/world"
    "hello world"
    "hello\nworld"
    42
    [ "hello" ]
  ];
  invalidPackages = [
    null
    { meta.mainProgram = "hello"; }
    (runner // { meta = { }; })
  ]
  ++ map (mainProgram: runner // { meta = { inherit mainProgram; }; }) invalidPrograms;
  manifest = packages.profile-images.passthru.manifest;
  fields = [
    "launcher"
    "profile_config"
    "ref"
    "source"
    "source_kind"
  ];
  imageTagLib = import ../../lib/util/image-tag.nix { };
  refPrefix = if pkgs.stdenv.hostPlatform.isDarwin then "" else "localhost/";
  entryMatches =
    image: launcher: entry:
    attrNames entry == fields
    && entry.ref == "${refPrefix}${image.imageName}:${imageTagLib.mkImageTag image}"
    && entry.source == toString image.source
    && entry.source_kind == image.source_kind
    && entry.profile_config == toString image.profileConfig
    && entry.launcher == "${launcher}/bin/wrix"
    && all hasContext [
      entry.source
      entry.profile_config
      entry.launcher
    ];
  customManifest = (wlib.mkProfileImages { custom = direct.image; }).passthru.manifest;
in
{
  "sandbox.agent-default-and-direct-contract" =
    selects "pi" piPackage (sandbox { }).image
    && selects "pi" piPackage (sandbox { agent = "pi"; }).image
    && selects "claude" imagePkgs.claude-code (sandbox { agent = "claude"; }).image
    && selects "direct" runner direct.image
    &&
      selects "direct" runner
        (sandbox {
          agent = "direct";
          agentPkg = runner;
          agentSettings = { };
        }).image
    && direct.image.claudeSettingsJson == null
    && direct.image.piSettingsJson == null
    && rejects { agent = "direct"; }
    && all (
      agentPkg:
      rejects {
        agent = "direct";
        inherit agentPkg;
      }
    ) invalidPackages
    && rejects {
      agent = "direct";
      agentPkg = runner;
      agentSettings.probe = true;
    }
    && rejects {
      agent = "unknown";
      agentPkg = runner;
    };

  "profiles.pi-default-outputs" =
    all (
      profile:
      let
        name = sandboxName profile;
        default = packages.${name};
        pi = packages."${name}-pi";
        claude = packages."${name}-claude";
      in
      default.drvPath == pi.drvPath
      && selects "pi" piPackage default.image
      && selects "claude" imagePkgs.claude-code claude.image
      && packages."image-${profile}".outPath == default.image.source.outPath
      && packages."image-${profile}-pi".outPath == pi.image.source.outPath
      && packages."image-${profile}-claude".outPath == claude.image.source.outPath
      && packages."${name}-mcp".drvPath == packages."${name}-pi-mcp".drvPath
      && selects "pi" piPackage packages."${name}-mcp".image
      && selects "claude" imagePkgs.claude-code packages."${name}-claude-mcp".image
      && default.meta.mainProgram == "wrix-run"
      && all (obsolete: !(hasAttr obsolete packages)) [
        "image-${profile}-direct"
        "${name}-direct"
        "${name}-direct-mcp"
      ]
    ) profiles
    && packages.default.drvPath == packages.sandbox-rust-pi.drvPath
    && packages.default.meta.mainProgram == "wrix-run";

  "profiles.pi-default-manifest" =
    attrNames manifest == profiles
    && !(hasAttr "profile-images-pi" packages)
    && all (
      profile:
      let
        package = packages.${sandboxName profile};
        entry = manifest.${profile}.pi;
      in
      attrNames manifest.${profile} == [ "pi" ]
      && selects "pi" piPackage package.image
      && entryMatches package.image package.launcher entry
      && entry.launcher != "${package}/bin/wrix"
    ) profiles
    && hasContext (toJSON manifest)
    && flake.devShells.${system}.default.LOOM_PROFILES_MANIFEST == toString packages.profile-images
    && attrNames customManifest.custom == [ "direct" ]
    && entryMatches direct.image direct.launcher customManifest.custom.direct;
}
