_:

{
  perSystem =
    {
      config,
      pkgs,
      wrix,
      ...
    }:
    {
      devShells.default = wrix.mkDevShell {
        profile = wrix.profiles.rust;

        env = {
          LOOM_PROFILES_MANIFEST = "${config.packages.profile-images-pi}";
          WRIX_AGENT = "pi";
        };

        packages = [
          config.packages.wrix
          config.treefmt.build.wrapper
          pkgs.flock
          pkgs.podman
          pkgs.skopeo
        ];
      };
    };
}
