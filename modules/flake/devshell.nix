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
          export PATH="${wrapix.devToolchain}/bin:$PATH"
          export RUSTC_WRAPPER="${pkgs.sccache}/bin/sccache"
          export SCCACHE_DIR="''${SCCACHE_DIR:-$HOME/.cache/sccache}"
          export SCCACHE_CACHE_SIZE="''${SCCACHE_CACHE_SIZE:-50G}"
          export CARGO_INCREMENTAL="''${CARGO_INCREMENTAL:-0}"
          # FR7: point git at versioned flock-wrapped hook shims
          if [ -e .git ]; then
            git config --local core.hooksPath lib/prek/hooks
          fi
          export LOOM_PROFILES_MANIFEST=${self'.packages.profile-images}
        '';

        packages = [
          wrapix.devToolchain
          pkgs.sccache
          config.treefmt.build.wrapper
          pkgs.cargo-nextest
          pkgs.flock
          pkgs.gh
          pkgs.podman
          self'.packages.loom
          self'.packages.sandbox-rust
          self'.packages.wrapix-notifyd
        ];
      };
    };
}
