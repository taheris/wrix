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
    "profile_config"
    "ref"
    "source"
    "source_kind"
  ];
  imageTagLib = import ../../lib/util/image-tag.nix { };
  refPrefix = if pkgs.stdenv.hostPlatform.isDarwin then "" else "localhost/";
  entryMatches =
    image: entry:
    attrNames entry == fields
    && entry.ref == "${refPrefix}${image.imageName}:${imageTagLib.mkImageTag image}"
    && entry.source == toString image.source
    && entry.source_kind == image.source_kind
    && entry.profile_config == toString image.profileConfig
    && all hasContext [
      entry.source
      entry.profile_config
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

  "profiles.image-only-output-contract" = all (
    profile:
    let
      default = packages."image-${profile}";
      pi = packages."image-${profile}-pi";
      claude = packages."image-${profile}-claude";
    in
    default.drvPath == pi.drvPath
    && selects "pi" piPackage default.image
    && selects "claude" imagePkgs.claude-code claude.image
    && !(hasAttr "image-${profile}-direct" packages)
  ) profiles;

  "profiles.host-cli-package-outputs" =
    packages.default.drvPath == packages.wrix.drvPath
    && packages.wrix.meta.mainProgram == "wrix"
    && all (
      name:
      !(pkgs.lib.hasPrefix "sandbox" name)
      && !(builtins.elem name [
        "debug"
        "wrix-run"
        "profile-images-pi"
      ])
    ) (attrNames packages);

  "sandbox.data-only-constructor" =
    let
      resolved = sandbox {
        profile = wlib.profiles.base;
        cpus = 2;
        memoryMb = 1024;
        deployKey = "example";
        packages = [ runner ];
        mounts = [
          {
            source = "/fixture";
            dest = "/fixture";
            mode = "ro";
            optional = true;
          }
        ];
        env.FOO = "bar";
        runtimeSecrets.TEST_TOKEN = "optional";
        mcp = { };
        mcpRuntime = false;
        agent = "direct";
        agentPkg = runner;
        agentSettings = { };
      };
    in
    attrNames resolved == [
      "devShell"
      "image"
      "profile"
    ]
    && resolved.image ? profileConfig
    && builtins.isFunction resolved.devShell
    && resolved.profile.env.FOO == "bar"
    && builtins.elem runner resolved.profile.packages
    && (builtins.head resolved.profile.mounts).optional;

  "profiles.data-only-manifest" =
    let
      missing = builtins.removeAttrs direct.image [ "profileConfig" ];
    in
    entryMatches direct.image customManifest.custom.direct
    && !(tryEval (builtins.deepSeq (wlib.mkProfileImages { invalid = missing; }).manifest true))
    .success;

  "profiles.pi-data-manifest" =
    attrNames manifest == profiles
    && !(hasAttr "profile-images-pi" packages)
    && all (
      profile:
      let
        image = packages.profile-images.sandboxes.${profile}.pi.image;
        entry = manifest.${profile}.pi;
      in
      attrNames manifest.${profile} == [ "pi" ] && image.agent == "pi" && entryMatches image entry
    ) profiles
    && hasContext (toJSON manifest)
    && flake.devShells.${system}.default.LOOM_PROFILES_MANIFEST == toString packages.profile-images
    && attrNames customManifest.custom == [ "direct" ]
    && entryMatches direct.image customManifest.custom.direct;
}
