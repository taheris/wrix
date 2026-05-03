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
        shellHook = ''
          ${wrapix.profiles.rust.shellHook}
          # FR7: point git at versioned flock-wrapped hook shims
          if [ -e .git ]; then
            git config --local core.hooksPath lib/prek/hooks
          fi
        '';

        packages = wrapix.profiles.rust.packages ++ [
          config.treefmt.build.wrapper
          pkgs.flock
          pkgs.gh
          pkgs.podman
          self'.packages.loom
          self'.packages.wrapix-notifyd
        ];
      };
    };
}
