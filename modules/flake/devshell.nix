_:

{
  perSystem =
    {
      config,
      pkgs,
      self',
      wrapix,
      city,
      ...
    }:
    {
      devShells.default = wrapix.mkDevShell {
        shellHook = ''
          ${city.shellHook}
          # FR7: point git at versioned flock-wrapped hook shims
          if [ -e .git ]; then
            git config --local core.hooksPath lib/prek/hooks
          fi
        '';

        packages = city.packages ++ [
          config.treefmt.build.wrapper
          pkgs.flock
          pkgs.gh
          pkgs.podman
          self'.packages.wrapix-notifyd
        ];
      };
    };
}
