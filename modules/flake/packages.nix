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
    in
    {
      packages = sandboxPkgs // {
        inherit (pkgs)
          beads
          beads-dolt
          beads-push
          gc
          pi-mono
          ;
        nodejs = linuxPkgs.nodejs_22;
        default = sandboxPkgs.sandbox-rust;
        city-config = city.configDir;
        loom = wrapix.loomPackage;
        ralph = wrapix.ralphPackage;
        tmux-mcp = import ../../lib/mcp/tmux/mcp-server.nix { inherit pkgs; };
        wrapix-builder = import ../../lib/builder { inherit pkgs linuxPkgs; };
        wrapix-notifyd = import ../../lib/notify/daemon.nix { inherit pkgs; };
      };
    };
}
