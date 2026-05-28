{ inputs, ... }:

{
  perSystem =
    { system, ... }:
    let
      inherit (inputs) nixpkgs;

      linuxSystem =
        if system == "aarch64-darwin" then
          "aarch64-linux"
        else if system == "x86_64-darwin" then
          "x86_64-linux"
        else
          system;

      beadsPkgs =
        hostPkgs_: linuxPkgs_:
        let
          m = import ../../lib/beads {
            pkgs = hostPkgs_;
            linuxPkgs = linuxPkgs_;
          };
        in
        {
          beads-dolt = m.dolt;
          beads-push = m.push;
        };

      # Override prek to 0.4.3 — nixpkgs nixos-unstable still pins 0.3.11,
      # which crashes during hook cleanup (indicatif Drop hits a poisoned
      # Mutex from a parallel worker, panic in destructor kills the hook
      # before its verdict reaches git push). Fixed upstream in 0.3.12+ via
      # indicatif 0.18.4 (j178/prek#1644). Drop this override and revert
      # lib/prek/hooks/{pre-commit,pre-push} to omit --no-progress once
      # nixpkgs catches up past 0.3.11.
      prekOverride = final: prev: {
        prek = prev.prek.overrideAttrs (_: rec {
          version = "0.4.3";
          src = final.fetchFromGitHub {
            owner = "j178";
            repo = "prek";
            tag = "v${version}";
            hash = "sha256-GseXLrklwVjrhgU3rfcBzM8ztnwjD631IgDGNb+4Plw=";
          };
          cargoDeps = final.rustPlatform.fetchCargoVendor {
            inherit src;
            name = "prek-${version}-vendor.tar.gz";
            hash = "sha256-gMbEHkKcUc4QqpWua3dczvlkKywwHEf+qxkn82Tzrys=";
          };
        });
      };

      # Agent runtime packages exposed to consumers:
      #
      #   pi-mono     — 0.72.1, pinned in lib/pi-mono/package.json
      #                 (RPC framing for the pi backend)
      #   claude-code — tracks nixos-unstable via flake.lock
      #                 (stream-json framing for the claude backend);
      #                 bumping nixpkgs may bump claude-code's wire surface.
      linuxOverlay =
        final: _prev:
        {
          pi-mono = final.callPackage ../../lib/pi-mono { };
        }
        // beadsPkgs final final;

      linuxPkgs = import nixpkgs {
        system = linuxSystem;
        overlays = [
          linuxOverlay
          prekOverride
        ];
        config.allowUnfree = true;
      };

      hostOverlay =
        final: _prev:
        {
          inherit (linuxPkgs) pi-mono;
        }
        // beadsPkgs final linuxPkgs;

    in
    {
      _module.args.pkgs = import nixpkgs {
        inherit system;
        overlays = [
          hostOverlay
          prekOverride
        ];
        config.allowUnfree = true;
      };

      _module.args.linuxPkgs = linuxPkgs;
    };
}
