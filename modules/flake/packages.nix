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
      inherit (builtins) mapAttrs;
      inherit (wrapix) profiles;

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
          profile = profiles.base;
          packages = [ linuxPkgs.podman ];
        };
      };
    in
    {
      packages = sandboxPkgs // {
        inherit (pkgs)
          beads
          beads-dolt
          beads-push
          gc
          ;
        default = sandboxPkgs.sandbox;
        city-config = city.configDir;
        loom = wrapix.loomPackage;
        ralph = wrapix.ralphPackage;
        tmux-mcp = import ../../lib/mcp/tmux/mcp-server.nix { inherit pkgs; };
        wrapix-builder = import ../../lib/builder { inherit pkgs linuxPkgs; };
        wrapix-notifyd = import ../../lib/notify/daemon.nix { inherit pkgs; };
      };
    };
}
