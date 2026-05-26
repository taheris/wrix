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

        shellHook = ''
          # Point git at versioned flock-wrapped hook shims (see specs/pre-commit.md).
          if [ -e .git ]; then
            git config --local core.hooksPath lib/prek/hooks
          fi
        '';

        packages = [
          config.treefmt.build.wrapper
          pkgs.cargo-nextest
          pkgs.flock
          pkgs.podman
          self'.packages.loom
          self'.packages.sandbox-rust
          self'.packages.wrapix-notifyd
        ];
      };
    };
}
