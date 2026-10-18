_:

{
  perSystem =
    {
      system,
      pkgs,
      test,
      wrix,
      linuxPkgs,
      linuxLoomCli,
      loomLib,
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
          profile = profiles.base;
        }
        {
          name = "rust";
          profile = profiles.rust;
        }
        {
          name = "python";
          profile = profiles.python;
        }
      ];
      mkSandboxFor =
        sandbox: extra:
        wrix.mkSandbox (
          {
            inherit (sandbox) profile;
          }
          // extra
          // {
            packages = [ linuxLoomCli ] ++ (extra.packages or [ ]);
          }
        );

      mkImagePackages = suffix: images: listToAttrs (map (mkImagePackage suffix images) sandboxes);
      mkImagePackage = suffix: images: p: {
        name = "image-${p.name}${suffix}";
        value = images.${p.name}.source // {
          image = images.${p.name};
        };
      };

      profileImages = mkProfileImages "pi";

      mkProfileImages =
        agent:
        listToAttrs (
          map (sandbox: {
            inherit (sandbox) name;
            value = (mkSandboxFor sandbox { inherit agent; }).image;
          }) sandboxes
        );

      imagePkgs = mkImagePackages "" profileImages;
      imageOverlays = listToAttrs (
        concatMap (agent: map (mkImagePackage "-${agent}" (mkProfileImages agent)) sandboxes) agents
      );

      isLinux = elem system [
        "aarch64-linux"
        "x86_64-linux"
      ];
      isDarwin = pkgs.stdenv.hostPlatform.isDarwin;
    in
    {
      packages =
        imagePkgs
        // imageOverlays
        // {
          inherit (pkgs) beads;
          test-image-git-credentials = test.testImages.gitCredentials;
          test-tmux-sandbox = test.tmuxTests.sandbox.image.source // {
            profileConfig = test.tmuxTests.sandbox.image.profileConfig;
          };

          default = wrix.rustPackage.wrix;
          nodejs = linuxPkgs.nodejs_22;
          profile-images = loomLib.mkProfileManifest {
            inherit pkgs;
            imagePkgs = linuxPkgs;
            wrixLib = wrix;
            loomBin = linuxLoomCli;
            runtimes.pi = { };
          };
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
          wrix = wrix.rustPackage.wrix;
          celler = wrix.rustPackage.celler;
          wrix-cache-hook = wrix.rustPackage.cacheHook;
          wrix-cache-publish = wrix.rustPackage.cachePublish;
          wrix-service-image = wrix.serviceImage;
          wrix-notify-host = import ../../lib/notify/client.nix {
            inherit pkgs;
            host = true;
          };
          wrix-notifyd = import ../../lib/notify/daemon.nix {
            inherit pkgs;
            focusBinary = wrix.rustPackage.focusNotification;
          };
          wrix-focus-notification = import ../../lib/notify/focus.nix {
            inherit pkgs;
            binary = wrix.rustPackage.focusNotification;
          };
        }
        // (
          if isDarwin then { wrix-builder = import ../../lib/builder { inherit pkgs linuxPkgs; }; } else { }
        );
    };
}
