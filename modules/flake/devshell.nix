_:

{
  perSystem =
    {
      config,
      pkgs,
      self',
      wrapix,
      ...
    }:
    {
      devShells.default = wrapix.mkDevShell {
        profile = wrapix.profiles.rust;

        packages = [
          config.treefmt.build.wrapper
          pkgs.cargo-nextest
          pkgs.flock
          pkgs.podman
          self'.packages.loom
        ];
      };
    };
}
