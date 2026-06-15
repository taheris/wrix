_:

{
  perSystem =
    {
      system,
      pkgs,
      test,
      wrix,
      linuxPkgs,
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
            profile = profiles.rust;
            packages = [ linuxPkgs.podman ];
          }).package;
      };
      sandboxOverlays = listToAttrs (
        concatMap (agent: mkProfileVariants "-${agent}" { inherit agent; }) agents
      );

      # Profile-agnostic launcher (`packages.wrix`) — no image baked in.
      # Orchestrators export WRIX_DEFAULT_IMAGE_REF/WRIX_DEFAULT_IMAGE_SOURCE
      # (or pass image_ref/image_source via SpawnConfig) before invoking it.
      wrixLauncher = (wrix.mkSandbox { profile = profiles.base; }).launcher;

      isLinux = elem system [
        "aarch64-linux"
        "x86_64-linux"
      ];

    in
    {
      packages =
        sandboxPkgs
        // sandboxOverlays
        // imagePkgs
        // imageOverlays
        // {
          inherit (pkgs)
            beads
            beads-dolt
            beads-push
            ;

          default = sandboxOverlays.sandbox-pi;
          nodejs = linuxPkgs.nodejs_22;
          profile-images = wrix.mkProfileImages profileImages;
          profile-images-pi = wrix.mkProfileImages profilePiImages;
        }
        // (
          # Test sandbox image (claude/beads stubbed out with `hello`);
          if isLinux then
            {
              test-image-base = test.testImages.base;
              test-image-base-perturbed = test.testImages.basePerturbed;
              test-image-nix = test.testImages.nix;
            }
          else
            { }
        )
        // {
          tmux-mcp = wrix.tmuxMcpPackage.bin;
          wrix = wrixLauncher;
          wrix-builder = import ../../lib/builder { inherit pkgs linuxPkgs; };
          wrix-notifyd = import ../../lib/notify/daemon.nix { inherit pkgs; };
        };
    };
}
