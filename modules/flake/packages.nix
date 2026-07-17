{ inputs, ... }:

{
  perSystem =
    {
      system,
      pkgs,
      test,
      wrix,
      linuxPkgs,
      treefmtWrapper,
      ...
    }:
    let
      inherit (builtins) concatMap elem listToAttrs;
      inherit (wrix) profiles;

      agents = [
        "claude"
        "pi"
      ];
      sandboxes = [
        {
          name = "base";
          package = "sandbox";
          profile = profiles.base;
        }
        {
          name = "rust";
          package = "sandbox-rust";
          profile = profiles.rust;
        }
        {
          name = "python";
          package = "sandbox-python";
          profile = profiles.python;
        }
      ];
      variants = [
        {
          suffix = "";
          extra = { };
        }
        {
          suffix = "-mcp";
          extra = {
            mcpRuntime = true;
          };
        }
      ];

      mkProfile = sandbox: suffix: extra: {
        name = "${sandbox.package}${suffix}";
        value = (mkSandboxFor sandbox extra).package;
      };
      mkProfileVariants =
        prefix: baseExtra:
        concatMap (
          variant:
          map (sandbox: mkProfile sandbox "${prefix}${variant.suffix}" (baseExtra // variant.extra)) sandboxes
        ) variants;
      mkSandboxFor = sandbox: extra: wrix.mkSandbox ({ inherit (sandbox) profile; } // extra);

      mkImagePackages = agent: images: listToAttrs (map (mkImagePackage agent images) sandboxes);
      mkImagePackage = agent: images: p: {
        name = "image-${p.name}${if agent == "direct" then "" else "-${agent}"}";
        value = images.${p.name};
      };

      profileImages = mkProfileImages "direct";
      profilePiImages = mkProfileImages "pi";

      mkProfileImages =
        agent:
        listToAttrs (
          map (sandbox: {
            inherit (sandbox) name;
            value = (mkSandboxFor sandbox { inherit agent; }).image;
          }) sandboxes
        );

      imagePkgs = mkImagePackages "direct" profileImages;
      imageOverlays = listToAttrs (
        concatMap (agent: map (mkImagePackage agent (mkProfileImages agent)) sandboxes) agents
      );
      sandboxPkgs = listToAttrs (mkProfileVariants "" { }) // {
        debug =
          (wrix.mkSandbox {
            agent = "pi";
            profile = profiles.rust;
            packages = [ linuxPkgs.podman ];
          }).package;
      };
      sandboxOverlays = listToAttrs (
        concatMap (agent: mkProfileVariants "-${agent}" { inherit agent; }) agents
      );

      isLinux = elem system [
        "aarch64-linux"
        "x86_64-linux"
      ];
      serviceProfiles = import ../../lib/sandbox/profiles.nix {
        pkgs = linuxPkgs;
        hostPkgs = linuxPkgs;
        inherit (inputs) crane fenix;
        treefmt = treefmtWrapper;
      };
      serviceRust = import ../../lib/services/rust.nix {
        pkgs = linuxPkgs;
        rustProfile = serviceProfiles.rust;
      };
      serviceImage = import ../../lib/services/image.nix {
        pkgs = linuxPkgs;
        hostPkgs = pkgs;
        inherit (serviceRust) cacheServe;
        asTarball = !isLinux;
      };

    in
    {
      packages =
        sandboxPkgs
        // sandboxOverlays
        // imagePkgs
        // imageOverlays
        // {
          inherit (pkgs) beads;

          default = sandboxOverlays.sandbox-rust-pi;
          nodejs = linuxPkgs.nodejs_22;
          profile-images = wrix.mkProfileImages profileImages;
          profile-images-pi = wrix.mkProfileImages profilePiImages;
        }
        // (
          # Test sandbox image (claude/beads stubbed out with `hello`);
          if isLinux then
            {
              test-image-base = test.testImages.base;
              test-image-base-direct = test.testImages.baseDirect;
              test-image-base-perturbed = test.testImages.basePerturbed;
              test-image-base-pi = test.testImages.basePi;
              test-image-nix = test.testImages.nix;
            }
          else
            { }
        )
        // {
          tmux-mcp = wrix.tmuxMcpPackage.bin;
          wrix = wrix.rustPackage.wrix;
          wrix-cache-hook = wrix.rustPackage.cacheHook;
          wrix-cache-publish = wrix.rustPackage.cachePublish;
          wrix-cache-serve = wrix.rustPackage.cacheServe;
          wrix-service-image = serviceImage;
          wrix-builder = import ../../lib/builder { inherit pkgs linuxPkgs; };
          wrix-notifyd = import ../../lib/notify/daemon.nix { inherit pkgs; };
        };
    };
}
