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

        packages = [
          config.treefmt.build.wrapper
          pkgs.flock
          pkgs.podman
          pkgs.skopeo
        ];
      };
    };
}
