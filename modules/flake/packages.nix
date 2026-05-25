_:

{
  perSystem =
    {
      pkgs,
      wrapix,
      city,
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
        # Direct runtime variants — agent=direct adds the loom-direct-runner
        # binary on top of the workspace profile. WRAPIX_AGENT=direct at
        # runtime selects the direct entrypoint branch.
        sandbox-direct = {
          profile = profiles.base;
          agent = "direct";
        };
        sandbox-rust-direct = {
          profile = profiles.rust;
          agent = "direct";
        };
        sandbox-python-direct = {
          profile = profiles.python;
          agent = "direct";
        };
        debug = {
          profile = profiles.rust;
          packages = [ linuxPkgs.podman ];
        };
      };

      # Profile-agnostic launcher (`packages.wrapix`) — no image baked in.
      # Loom and other orchestrators export
      # WRAPIX_DEFAULT_IMAGE_REF/WRAPIX_DEFAULT_IMAGE_SOURCE (or pass
      # image_ref/image_source via SpawnConfig) before invoking it.
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
          nodejs = linuxPkgs.nodejs_22;
          default = sandboxPkgs.sandbox-rust;
          city-config = city.configDir;
          loom = wrapix.loomPackage.bin;
          profile-images = wrapix.mkProfileImages profileImages;
          ralph = wrapix.ralphPackage;
          tmux-mcp = wrapix.tmuxMcpPackage.bin;
          wrapix = wrapixLauncher;
          wrapix-builder = import ../../lib/builder { inherit pkgs linuxPkgs; };
          wrapix-notifyd = import ../../lib/notify/daemon.nix { inherit pkgs; };
        };
    };
}
