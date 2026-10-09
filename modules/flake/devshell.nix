_:

{
  perSystem =
    {
      config,
      pkgs,
      wrix,
      linuxLoomCli,
      ...
    }:
    let
      sandbox = wrix.mkSandbox {
        profile = wrix.profiles.rust;
        agent = "pi";
        packages = [ linuxLoomCli ];
      };
    in
    {
      devShells.default = sandbox.devShell {
        env = {
          LOOM_PROFILES_MANIFEST = "${config.packages.profile-images}";
          WRIX_AGENT = "pi";
        };

        packages = [
          config.packages.loom
          config.treefmt.build.wrapper
          pkgs.flock
          pkgs.podman
          pkgs.skopeo
        ];
      };
    };
}
