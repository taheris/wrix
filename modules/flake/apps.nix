_:

{
  perSystem =
    {
      pkgs,
      system,
      wrapix,
      city,
      test,
      ...
    }:
    let
      inherit (pkgs) lib;
      isLinux = lib.elem system [
        "x86_64-linux"
        "aarch64-linux"
      ];

      # On-demand cargo-fuzz runner. Not gated by `nix flake check` per
      # specs/loom-tests.md NFR §Property-Based Testing — proptest covers
      # invariants in CI; fuzz runs are nightly/manual. Linux-only because
      # cargo-fuzz needs nightly LLVM sanitizers.
      fuzzLoom = pkgs.writeShellApplication {
        name = "fuzz-loom";
        runtimeInputs = [
          pkgs.cargo-fuzz
          pkgs.cargo
          pkgs.rustc
        ];
        text = ''
          set -euo pipefail

          repo_root="''${WRAPIX_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
          cd "$repo_root/loom"

          if [ ! -d fuzz ]; then
            echo "fuzz-loom: no fuzz targets defined under loom/fuzz/." >&2
            echo "  bootstrap:  cd loom && cargo +nightly fuzz init" >&2
            echo "  add target: cargo +nightly fuzz add <target>" >&2
            echo "  then re-run: nix run .#fuzz-loom -- <target>" >&2
            exit 1
          fi

          exec cargo +nightly fuzz run "$@"
        '';
      };
    in
    {
      apps = {
        city = city.app;
        init = wrapix.ralphInitApp;
        ralph = city.ralph.app;
        test = test.app;
        test-city = test.apps.city;
        test-ralph = test.apps.ralph;
        test-ralph-container = test.apps.ralph-container;
      }
      // lib.optionalAttrs isLinux {
        fuzz-loom = {
          meta.description = "On-demand cargo-fuzz runner for loom (nightly toolchain)";
          type = "app";
          program = "${fuzzLoom}/bin/fuzz-loom";
        };
      };
    };
}
