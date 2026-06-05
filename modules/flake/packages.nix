_:

{
  perSystem =
    {
      system,
      pkgs,
      test,
      wrapix,
      linuxPkgs,
      ...
    }:
    let
      inherit (builtins) elem listToAttrs mapAttrs;
      inherit (wrapix) profiles;

      isLinux = elem system [
        "aarch64-linux"
        "x86_64-linux"
      ];

      builtInProfiles = { inherit (profiles) base rust python; };
      profileSandboxes = mapAttrs (_: profile: wrapix.mkSandbox { inherit profile; }) builtInProfiles;
      profileImages = mapAttrs (_: s: s.image) profileSandboxes;

      # `packages.image-<name>` — per-profile OCI artifact for the default
      # direct runtime. Agent overlays use `image-<name>-claude` and
      # `image-<name>-pi`.
      imagePkgs = listToAttrs (
        map (name: {
          name = "image-${name}";
          value = profileImages.${name};
        }) (builtins.attrNames builtInProfiles)
      );
      overlayImagePkgs = listToAttrs (
        builtins.concatMap
          (
            agent:
            map (name: {
              name = "image-${name}-${agent}";
              value =
                (wrapix.mkSandbox {
                  profile = builtInProfiles.${name};
                  inherit agent;
                }).image;
            }) (builtins.attrNames builtInProfiles)
          )
          [
            "claude"
            "pi"
          ]
      );

      mkSandboxPkg = cfg: (wrapix.mkSandbox cfg).package;
      sandboxPkgs = mapAttrs (_: mkSandboxPkg) {
        sandbox = {
          profile = profiles.base;
        };
        sandbox-rust = {
          profile = profiles.rust;
        };
        sandbox-python = {
          profile = profiles.python;
        };
        sandbox-mcp = {
          profile = profiles.base;
          mcpRuntime = true;
        };
        sandbox-rust-mcp = {
          profile = profiles.rust;
          mcpRuntime = true;
        };
        sandbox-python-mcp = {
          profile = profiles.python;
          mcpRuntime = true;
        };
        debug = {
          profile = profiles.rust;
          packages = [ linuxPkgs.podman ];
        };
      };
      sandboxOverlayPkgs = mapAttrs (_: mkSandboxPkg) {
        sandbox-claude = {
          profile = profiles.base;
          agent = "claude";
        };
        sandbox-pi = {
          profile = profiles.base;
          agent = "pi";
        };
        sandbox-rust-claude = {
          profile = profiles.rust;
          agent = "claude";
        };
        sandbox-rust-pi = {
          profile = profiles.rust;
          agent = "pi";
        };
        sandbox-python-claude = {
          profile = profiles.python;
          agent = "claude";
        };
        sandbox-python-pi = {
          profile = profiles.python;
          agent = "pi";
        };
        sandbox-mcp-claude = {
          profile = profiles.base;
          mcpRuntime = true;
          agent = "claude";
        };
        sandbox-mcp-pi = {
          profile = profiles.base;
          mcpRuntime = true;
          agent = "pi";
        };
        sandbox-rust-mcp-claude = {
          profile = profiles.rust;
          mcpRuntime = true;
          agent = "claude";
        };
        sandbox-rust-mcp-pi = {
          profile = profiles.rust;
          mcpRuntime = true;
          agent = "pi";
        };
        sandbox-python-mcp-claude = {
          profile = profiles.python;
          mcpRuntime = true;
          agent = "claude";
        };
        sandbox-python-mcp-pi = {
          profile = profiles.python;
          mcpRuntime = true;
          agent = "pi";
        };
      };

      # Profile-agnostic launcher (`packages.wrapix`) — no image baked in.
      # Orchestrators export WRAPIX_DEFAULT_IMAGE_REF/WRAPIX_DEFAULT_IMAGE_SOURCE
      # (or pass image_ref/image_source via SpawnConfig) before invoking it.
      wrapixLauncher = (wrapix.mkSandbox { profile = profiles.base; }).launcher;
    in
    {
      packages =
        sandboxPkgs
        // sandboxOverlayPkgs
        // imagePkgs
        // overlayImagePkgs
        // {
          inherit (pkgs)
            beads
            beads-dolt
            beads-push
            ;

          default = sandboxOverlayPkgs.sandbox-pi;
          nodejs = linuxPkgs.nodejs_22;
          profile-images = wrapix.mkProfileImages profileImages;
          # Test sandbox image (claude/beads stubbed out with `hello`);
          # consumed by the host-side podman verifiers in tests/sandbox/.
          # Linux-only: the image builds against linuxPkgs and the podman
          # verifiers only run on Linux hosts.
        }
        // (
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
          tmux-mcp = wrapix.tmuxMcpPackage.bin;
          wrapix = wrapixLauncher;
          wrapix-builder = import ../../lib/builder { inherit pkgs linuxPkgs; };
          wrapix-notifyd = import ../../lib/notify/daemon.nix { inherit pkgs; };
        };
    };
}
