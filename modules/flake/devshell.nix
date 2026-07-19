_:

{
  perSystem =
    {
      config,
      pkgs,
      wrix,
      ...
    }:
    let
      sandbox = wrix.mkSandbox {
        profile = wrix.profiles.rust;
        agent = "pi";
      };
    in
    {
      devShells.default = sandbox.devShell {
        env = {
          LOOM_PROFILES_MANIFEST = "${config.packages.profile-images-pi}";
          WRIX_AGENT = "pi";
        };

        packages = [
          config.treefmt.build.wrapper
          pkgs.flock
          pkgs.podman
          pkgs.skopeo
        ];
      };
    };
}
