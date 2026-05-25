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
          # Point git at versioned flock-wrapped hook shims (see specs/pre-commit.md).
          if [ -e .git ]; then
            git config --local core.hooksPath lib/prek/hooks
          fi
        '';

        packages = [
          wrapix.devToolchain
          pkgs.sccache
          config.treefmt.build.wrapper
          pkgs.cargo-nextest
          pkgs.flock
          pkgs.gh
          pkgs.podman
          self'.packages.sandbox-rust
          self'.packages.wrapix-notifyd
        ];
      };
    };
}
