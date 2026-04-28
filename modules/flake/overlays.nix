{ inputs, ... }:

{
  perSystem =
    { system, ... }:
    let
      inherit (inputs) nixpkgs;

      linuxSystem = if system == "aarch64-darwin" then "aarch64-linux" else system;

      wrapixBeadsPkgs =
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

      beadsFor =
        pkgs':
        pkgs'.callPackage "${inputs.beads}/default.nix" {
          self = inputs.beads;
          buildGoModule =
            args:
            pkgs'.buildGoModule (
              args
              // {
                proxyVendor = true;
                vendorHash = "sha256-S/NavjGH6VSPU+rCtqtviOcGhgXc6VZUXCUhasSdUGU=";
              }
            );
        };

      gcFor =
        pkgs':
        pkgs'.buildGo126Module {
          pname = "gc";
          version = "dev";
          src = inputs.gascity;
          subPackages = [ "cmd/gc" ];
          proxyVendor = true;
          vendorHash = "sha256-59k7xFBaLZJ50KWNhwIzttE8j7GXZPneq6o4eUTlvBI=";
          doCheck = false;
        };

      linuxOverlay =
        final: _prev:
        {
          beads = beadsFor final;
          gc = gcFor final;
        }
        // wrapixBeadsPkgs final final;

      linuxPkgs = import nixpkgs {
        system = linuxSystem;
        overlays = [ linuxOverlay ];
        config.allowUnfree = true;
      };

      hostOverlay =
        final: _prev:
        {
          beads = beadsFor final;
          gc = gcFor final;
        }
        // wrapixBeadsPkgs final linuxPkgs;
    in
    {
      _module.args.pkgs = import nixpkgs {
        inherit system;
        overlays = [ hostOverlay ];
        config.allowUnfree = true;
      };

      _module.args.linuxPkgs = linuxPkgs;
    };
}
