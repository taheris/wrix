{ inputs, ... }:

{
  perSystem =
    {
      system,
      pkgs,
      wrapix,
      linuxPkgs,
      ...
    }:
    let
      inherit (builtins) listToAttrs mapAttrs;
      inherit (wrapix) profiles;

      builtInProfiles = { inherit (profiles) base rust python; };
      profileSandboxes = mapAttrs (_: profile: wrapix.mkSandbox { inherit profile; }) builtInProfiles;
      profileImages = mapAttrs (_: s: s.image) profileSandboxes;

      # `packages.image-<name>` — per-profile OCI artifact (claude + pi
      # runtimes both installed; WRAPIX_AGENT picks at container start).
      imagePkgs = listToAttrs (
        map (name: {
          name = "image-${name}";
          value = profileImages.${name};
        }) (builtins.attrNames builtInProfiles)
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
        # Pi runtime variants — same workspace profiles, agent=pi adds pi-mono
        # (Node.js + pi binary) on top. WRAPIX_AGENT=pi at runtime selects the
        # pi entrypoint branch.
        sandbox-pi = {
          profile = profiles.base;
          agent = "pi";
        };
        sandbox-rust-pi = {
          profile = profiles.rust;
          agent = "pi";
        };
        sandbox-python-pi = {
          profile = profiles.python;
          agent = "pi";
        };
        debug = {
          profile = profiles.rust;
          packages = [ linuxPkgs.podman ];
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
        // imagePkgs
        // {
          inherit (pkgs)
            beads
            beads-dolt
            beads-push
            pi-mono
            ;

          default = sandboxPkgs.sandbox-rust;
          loom = inputs.loom.packages.${system}.loom;
          nodejs = linuxPkgs.nodejs_22;
          profile-images = wrapix.mkProfileImages profileImages;
          tmux-mcp = wrapix.tmuxMcpPackage.bin;
          wrapix = wrapixLauncher;
          wrapix-builder = import ../../lib/builder { inherit pkgs linuxPkgs; };
          wrapix-notifyd = import ../../lib/notify/daemon.nix { inherit pkgs; };
        };
    };
}
